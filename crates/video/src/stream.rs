//! Silent playback: raw frames out of a running `ffmpeg` child, onto a bounded queue (issue #73).
//!
//! `ffmpeg` is asked for the clip from a start time as raw RGBA frames of an exact size (`-f rawvideo -pix_fmt rgba`, paced at
//! the clip's own speed with `-re`). A worker thread reads the child's standard output in whole-frame chunks and puts each
//! frame on a queue of [`QUEUE_FRAMES`]. The consumer (the engine, once per UI frame) takes them with [`FrameStream::poll`].
//!
//! * **Back-pressure.** The worker never blocks on the consumer: a frame that finds the queue full is dropped and counted
//!   ([`FrameStream::dropped`]), so a slow UI shows later frames, never a growing backlog or a stalled decoder. A frame's time
//!   comes from its index (the child is told the frame rate), so a dropped frame does not shift the ones after it.
//! * **Cancellation.** Dropping the stream (or [`FrameStream::stop`], or the cancel flag of [`crate::Limits`]) stops it: the
//!   child is killed with its whole process tree, which closes its pipes, which ends the worker. Nothing is left running.
//! * **Bounds.** A frame is `width * height * 4` bytes of an exact, capped size ([`crate::MAX_FRAME_EDGE`]); the queue holds a
//!   handful; the child's error output is kept to its last [`MAX_STDERR`] bytes. No deadline applies to a stream (a clip plays
//!   as long as it is long); stopping it is the caller's job.
//!
//! The child is behind [`Kill`], so the reading and the queue are tested with in-memory readers and a mock child.

use std::io::{ErrorKind, Read};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::VideoError;

/// Frames the queue holds between the worker and the consumer.
pub const QUEUE_FRAMES: usize = 3;
/// The end of the child's error output that a failure keeps.
pub const MAX_STDERR: usize = 4096;
/// The frame rate a clip is played at when its own is not known.
pub const FALLBACK_FPS: f64 = 30.0;
/// The highest frame rate a stream is asked for.
pub const MAX_FPS: f64 = 120.0;
/// How long the worker waits for the child's error output to end after its video did.
const STDERR_GRACE: Duration = Duration::from_millis(250);

/// One decoded frame: `width * height` RGBA pixels, upright, sRGB-encoded.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// 0 for the first frame of the stream, counting dropped ones.
    pub index: u64,
    /// Where it belongs in the clip, in seconds: the stream's start plus `index / fps`.
    pub pts: f64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// What stops the child behind a stream. Must end the process tree and be safe to call twice.
pub trait Kill: Send {
    fn kill(&mut self);
}

/// What a stream delivers: the geometry and timing its frames have.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StreamSpec {
    /// Seconds into the clip the first frame is from.
    pub start: f64,
    /// Frames per second of the stream (after any cap).
    pub fps: f64,
    pub width: u32,
    pub height: u32,
}

impl StreamSpec {
    /// Bytes of one frame, or `None` when the size is nothing or too large to be real.
    pub fn frame_len(&self) -> Option<usize> {
        let len = u64::from(self.width).checked_mul(u64::from(self.height))?.checked_mul(4)?;
        let cap = (crate::MAX_FRAME_EDGE as u64) * (crate::MAX_FRAME_EDGE as u64) * 4;
        (len > 0 && len <= cap).then_some(len as usize)
    }

    /// Where frame `index` belongs in the clip.
    pub fn pts(&self, index: u64) -> f64 {
        self.start + index as f64 / self.fps
    }
}

/// What the consumer finds when it asks for the next frame.
#[derive(Debug)]
pub enum Poll {
    Frame(Frame),
    /// Nothing yet; the stream is still running.
    Empty,
    /// The stream is over: `Ok` after the last frame of the clip, else why not. Said once; later polls say `Ok`.
    Ended(Result<(), VideoError>),
}

#[derive(Default)]
struct Shared {
    /// Set to stop the worker at its next frame.
    stop: AtomicBool,
    /// Frames the worker produced / dropped because the queue was full.
    produced: AtomicU64,
    dropped: AtomicU64,
    finished: AtomicBool,
    end: Mutex<Option<Result<(), VideoError>>>,
    stderr: Mutex<Vec<u8>>,
}

/// A running stream of frames. Dropping it kills the child.
pub struct FrameStream {
    rx: Receiver<Frame>,
    shared: Arc<Shared>,
    control: Box<dyn Kill>,
    spec: StreamSpec,
    over: bool,
}

impl std::fmt::Debug for FrameStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameStream").field("spec", &self.spec).field("dropped", &self.dropped()).finish()
    }
}

impl FrameStream {
    /// Read frames of `spec` from `stdout` (the child's standard output) on a worker thread, with `stderr` drained beside it,
    /// `queue` frames deep. `control` ends the child; `cancel` is the caller's own stop flag. On an error the child is killed.
    pub fn from_parts(
        stdout: Box<dyn Read + Send>,
        stderr: Option<Box<dyn Read + Send>>,
        mut control: Box<dyn Kill>,
        spec: StreamSpec,
        queue: usize,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<FrameStream, VideoError> {
        let Some(frame_len) = spec.frame_len() else {
            control.kill();
            return Err(VideoError::Invalid(format!("a {}x{} frame is not a size to stream", spec.width, spec.height)));
        };
        if !(spec.fps.is_finite() && spec.fps > 0.0 && spec.start.is_finite()) {
            control.kill();
            return Err(VideoError::Invalid("the stream needs a positive frame rate and a start time".into()));
        }
        let shared = Arc::new(Shared::default());
        let (tx, rx) = mpsc::sync_channel::<Frame>(queue.max(1));
        let (err_done_tx, err_done_rx) = mpsc::channel::<()>();
        if let Some(stderr) = stderr {
            let shared = shared.clone();
            let spawned = std::thread::Builder::new().name("lc-video-stderr".into()).spawn(move || {
                drain_tail(stderr, &shared.stderr);
                let _ = err_done_tx.send(());
            });
            if let Err(e) = spawned {
                control.kill();
                return Err(VideoError::Failed(format!("could not start the video reader: {e}")));
            }
        } else {
            drop(err_done_tx);
        }
        let worker_shared = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("lc-video-frames".into())
            .spawn(move || worker(stdout, spec, frame_len, tx, &worker_shared, cancel.as_deref(), &err_done_rx));
        if let Err(e) = spawned {
            control.kill();
            return Err(VideoError::Failed(format!("could not start the video reader: {e}")));
        }
        Ok(FrameStream { rx, shared, control, spec, over: false })
    }

    pub fn spec(&self) -> StreamSpec {
        self.spec
    }

    /// Frames dropped so far because the consumer was behind.
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// Frames the worker has produced so far (queued or dropped).
    pub fn produced(&self) -> u64 {
        self.shared.produced.load(Ordering::Relaxed)
    }

    /// Whether the worker has finished (the child's video ended or it was stopped); frames may still be queued.
    pub fn is_finished(&self) -> bool {
        self.shared.finished.load(Ordering::Acquire)
    }

    /// The next frame, if one is ready, without waiting.
    pub fn poll(&mut self) -> Poll {
        let got = self.rx.try_recv();
        self.settle(got)
    }

    /// [`FrameStream::poll`], waiting up to `timeout` for a frame.
    pub fn wait(&mut self, timeout: Duration) -> Poll {
        let got = self.rx.recv_timeout(timeout).map_err(|e| match e {
            RecvTimeoutError::Timeout => TryRecvError::Empty,
            RecvTimeoutError::Disconnected => TryRecvError::Disconnected,
        });
        self.settle(got)
    }

    fn settle(&mut self, got: Result<Frame, TryRecvError>) -> Poll {
        match got {
            Ok(frame) => Poll::Frame(frame),
            Err(TryRecvError::Empty) => Poll::Empty,
            Err(TryRecvError::Disconnected) => {
                if std::mem::replace(&mut self.over, true) {
                    return Poll::Ended(Ok(()));
                }
                let end = self.shared.end.lock().unwrap_or_else(PoisonError::into_inner).take();
                Poll::Ended(end.unwrap_or(Ok(())))
            }
        }
    }

    /// Stop now: the child is killed and the worker ends. Frames already queued can still be polled. Dropping does the same.
    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.control.kill();
    }
}

impl Drop for FrameStream {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Keep the last [`MAX_STDERR`] bytes `reader` yields into `tail`, until it ends.
fn drain_tail(mut reader: impl Read, tail: &Mutex<Vec<u8>>) {
    let mut buf = [0u8; 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                let mut kept = tail.lock().unwrap_or_else(PoisonError::into_inner);
                kept.extend_from_slice(buf.get(..n).unwrap_or_default());
                if kept.len() > MAX_STDERR {
                    let cut = kept.len() - MAX_STDERR;
                    kept.drain(..cut);
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

/// How a read of frames ended.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ReadEnd {
    /// Whole frames handed on.
    pub frames: u64,
    /// Bytes of a last frame that never completed (dropped).
    pub partial: usize,
    pub io_error: Option<String>,
    /// The consumer asked to stop.
    pub stopped: bool,
}

/// Read `frame_len`-byte frames from `reader` until it ends, handing each (with its index) to `each`, which says whether to go
/// on. A read gives any number of bytes; a frame is only ever handed on whole, and a short last one is counted, not delivered.
pub(crate) fn read_frames<R: Read>(mut reader: R, frame_len: usize, mut each: impl FnMut(u64, Vec<u8>) -> bool) -> ReadEnd {
    let mut end = ReadEnd::default();
    loop {
        let mut buf = vec![0u8; frame_len];
        let mut filled = 0usize;
        while filled < frame_len {
            let Some(room) = buf.get_mut(filled..) else { return end };
            match reader.read(room) {
                Ok(0) => {
                    end.partial = filled;
                    return end;
                }
                Ok(n) => filled = filled.saturating_add(n).min(frame_len),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => {
                    end.partial = filled;
                    end.io_error = Some(e.to_string());
                    return end;
                }
            }
        }
        let index = end.frames;
        end.frames = end.frames.saturating_add(1);
        if !each(index, buf) {
            end.stopped = true;
            return end;
        }
    }
}

fn worker(
    stdout: Box<dyn Read + Send>,
    spec: StreamSpec,
    frame_len: usize,
    tx: SyncSender<Frame>,
    shared: &Shared,
    cancel: Option<&AtomicBool>,
    err_done: &Receiver<()>,
) {
    let cancelled = || cancel.is_some_and(|c| c.load(Ordering::Acquire));
    let end = read_frames(stdout, frame_len, |index, rgba| {
        if shared.stop.load(Ordering::Acquire) || cancelled() {
            return false;
        }
        shared.produced.fetch_add(1, Ordering::Relaxed);
        let frame = Frame { index, pts: spec.pts(index), width: spec.width, height: spec.height, rgba };
        match tx.try_send(frame) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                shared.dropped.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    });
    // the child's error output ends with the child; a straggler holding it must not hold this thread
    let _ = err_done.recv_timeout(STDERR_GRACE);
    let tail = tail_text(&shared.stderr.lock().unwrap_or_else(PoisonError::into_inner));
    let outcome = if cancelled() {
        Err(VideoError::Cancelled)
    } else if shared.stop.load(Ordering::Acquire) {
        Ok(())
    } else if let Some(e) = end.io_error {
        Err(VideoError::Failed(if tail.is_empty() { e } else { tail }))
    } else if end.frames == 0 {
        Err(if tail.is_empty() { VideoError::NoFrame } else { VideoError::Failed(tail) })
    } else if end.partial > 0 && !tail.is_empty() {
        // the child died in the middle of a frame and said why
        Err(VideoError::Failed(tail))
    } else {
        Ok(())
    };
    *shared.end.lock().unwrap_or_else(PoisonError::into_inner) = Some(outcome);
    shared.finished.store(true, Ordering::Release);
}

fn tail_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

/// The frame rate a clip is streamed at: its own (`frame_rate`, [`FALLBACK_FPS`] when unknown), held to `cap` when there is one,
/// and to 1 to [`MAX_FPS`].
pub fn playback_fps(frame_rate: Option<f64>, cap: Option<f64>) -> f64 {
    let rate = frame_rate.filter(|r| r.is_finite() && *r > 0.0).unwrap_or(FALLBACK_FPS);
    let rate = match cap.filter(|c| c.is_finite() && *c > 0.0) {
        Some(c) => rate.min(c),
        None => rate,
    };
    rate.clamp(1.0, MAX_FPS)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    use super::*;

    /// A reader that gives at most `chunk` bytes per read, whatever is asked for.
    struct Trickle<R> {
        inner: R,
        chunk: usize,
    }

    impl<R: Read> Read for Trickle<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.chunk);
            self.inner.read(buf.get_mut(..n).unwrap_or_default())
        }
    }

    fn spec(w: u32, h: u32) -> StreamSpec {
        StreamSpec { start: 10.0, fps: 25.0, width: w, height: h }
    }

    /// `frames` frames of 2x1 pixels (8 bytes) whose every byte is the frame's number.
    fn clip(frames: u8) -> Vec<u8> {
        (0..frames).flat_map(|i| [i; 8]).collect()
    }

    struct NoKill;
    impl Kill for NoKill {
        fn kill(&mut self) {}
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn a_byte_stream_is_cut_into_exact_frames_whatever_the_read_sizes() {
        for chunk in [1, 3, 7, 8, 9, 64] {
            let mut data = clip(3);
            data.extend_from_slice(&[9, 9, 9]); // a frame that never finished
            let mut got = Vec::new();
            let end = read_frames(Trickle { inner: Cursor::new(data), chunk }, 8, |i, f| {
                got.push((i, f));
                true
            });
            assert_eq!((end.frames, end.partial, end.stopped, end.io_error), (3, 3, false, None), "chunk {chunk}");
            assert_eq!(got.len(), 3);
            for (n, (i, f)) in got.iter().enumerate() {
                assert_eq!((*i, f.len(), f.iter().all(|b| usize::from(*b) == n)), (n as u64, 8, true), "chunk {chunk}, frame {n}");
            }
        }
    }

    #[test]
    fn a_partial_only_stream_and_an_empty_one_give_no_frames() {
        let end = read_frames(Cursor::new(vec![1u8; 5]), 8, |_, _| true);
        assert_eq!((end.frames, end.partial), (0, 5));
        let end = read_frames(Cursor::new(Vec::new()), 8, |_, _| true);
        assert_eq!((end.frames, end.partial), (0, 0));
        let end = read_frames(Cursor::new(clip(5)), 8, |i, _| i < 1);
        assert_eq!((end.frames, end.stopped), (2, true), "the consumer stops the read");
    }

    #[test]
    fn a_failing_read_ends_the_stream_with_what_it_had() {
        struct Breaks(usize);
        impl Read for Breaks {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.0 == 0 {
                    return Err(std::io::Error::other("pipe broke"));
                }
                self.0 -= 1;
                buf.fill(7);
                Ok(buf.len().min(8))
            }
        }
        let end = read_frames(Breaks(2), 8, |_, _| true);
        assert_eq!((end.frames, end.partial), (2, 0));
        assert_eq!(end.io_error.as_deref(), Some("pipe broke"));
    }

    #[test]
    fn a_stream_delivers_frames_in_order_with_their_times_then_ends() {
        let mut data = clip(4);
        data.extend_from_slice(&[1, 2, 3]);
        let mut s = FrameStream::from_parts(Box::new(Cursor::new(data)), None, Box::new(NoKill), spec(2, 1), 8, None).unwrap();
        let mut frames = Vec::new();
        let end = loop {
            match s.wait(Duration::from_secs(10)) {
                Poll::Frame(f) => frames.push(f),
                Poll::Empty => panic!("the stream stalled"),
                Poll::Ended(r) => break r,
            }
        };
        assert!(end.is_ok(), "a short last frame is dropped, not an error: {end:?}");
        assert_eq!(frames.len(), 4);
        for (n, f) in frames.iter().enumerate() {
            assert_eq!((f.index, f.width, f.height, f.rgba.len()), (n as u64, 2, 1, 8));
            assert!((f.pts - (10.0 + n as f64 / 25.0)).abs() < 1e-9, "{}", f.pts);
        }
        assert!(matches!(s.poll(), Poll::Ended(Ok(()))), "said again, not an error");
        assert_eq!((s.produced(), s.dropped()), (4, 0));
    }

    #[test]
    fn a_slow_consumer_loses_frames_not_the_decoder_and_the_times_do_not_shift() {
        let mut s = FrameStream::from_parts(Box::new(Cursor::new(clip(10))), None, Box::new(NoKill), spec(2, 1), 2, None).unwrap();
        wait_until("the worker to read the whole clip with nobody listening", || s.is_finished());
        assert_eq!(s.dropped(), 8, "a queue of two holds the first two; the other eight are dropped");
        let mut kept = Vec::new();
        let end = loop {
            match s.poll() {
                Poll::Frame(f) => kept.push(f.index),
                Poll::Empty => panic!("a finished stream has nothing pending"),
                Poll::Ended(r) => break r,
            }
        };
        assert!(end.is_ok());
        assert_eq!(kept, vec![0, 1]);
        // a consumer that keeps up loses nothing
        let mut s = FrameStream::from_parts(Box::new(Cursor::new(clip(10))), None, Box::new(NoKill), spec(2, 1), 2, None).unwrap();
        let mut seen = Vec::new();
        loop {
            match s.wait(Duration::from_secs(10)) {
                Poll::Frame(f) => seen.push(f.index),
                Poll::Empty => {}
                Poll::Ended(_) => break,
            }
        }
        assert!(seen.windows(2).all(|w| w[0] < w[1]), "in order: {seen:?}");
        assert_eq!(seen.len() as u64 + s.dropped(), 10, "every frame is delivered or counted");
    }

    /// A child stand-in: the video pipe stays open until `kill` closes it, as the pipe of a killed process does.
    struct Pipe {
        rx: Receiver<Vec<u8>>,
        pending: Vec<u8>,
        closed: Arc<AtomicBool>,
    }
    impl Read for Pipe {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pending.is_empty() {
                match self.rx.recv() {
                    Ok(bytes) => self.pending = bytes,
                    Err(_) => return Ok(0),
                }
            }
            let n = buf.len().min(self.pending.len());
            buf.get_mut(..n).unwrap_or_default().copy_from_slice(self.pending.get(..n).unwrap_or_default());
            self.pending.drain(..n);
            Ok(n)
        }
    }
    impl Drop for Pipe {
        fn drop(&mut self) {
            self.closed.store(true, Ordering::Release);
        }
    }
    struct MockChild {
        kills: Arc<AtomicUsize>,
        write: Option<mpsc::Sender<Vec<u8>>>,
    }
    impl Kill for MockChild {
        fn kill(&mut self) {
            self.kills.fetch_add(1, Ordering::SeqCst);
            self.write = None; // the pipe's writer end goes with the process
        }
    }

    fn mock() -> (Box<Pipe>, MockChild, Arc<AtomicUsize>, Arc<AtomicBool>, mpsc::Sender<Vec<u8>>) {
        let (tx, rx) = mpsc::channel();
        let (kills, closed) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
        let pipe = Box::new(Pipe { rx, pending: Vec::new(), closed: closed.clone() });
        (pipe, MockChild { kills: kills.clone(), write: Some(tx.clone()) }, kills, closed, tx)
    }

    #[test]
    fn dropping_the_stream_kills_the_child_and_ends_the_worker() {
        let (pipe, child, kills, closed, writer) = mock();
        let mut s = FrameStream::from_parts(pipe, None, Box::new(child), spec(2, 1), 2, None).unwrap();
        writer.send(clip(1)).unwrap();
        assert!(matches!(s.wait(Duration::from_secs(10)), Poll::Frame(_)));
        assert_eq!(kills.load(Ordering::SeqCst), 0, "a running stream is left alone");
        // the test's own sender would keep the pipe open past the kill (the real writer end dies with the child)
        drop(writer);
        drop(s);
        assert_eq!(kills.load(Ordering::SeqCst), 1, "dropping kills the child");
        wait_until("the worker to end and let go of the pipe", || closed.load(Ordering::Acquire));
    }

    #[test]
    fn stopping_twice_and_then_dropping_is_harmless() {
        let (pipe, child, kills, closed, writer) = mock();
        drop(writer);
        let mut s = FrameStream::from_parts(pipe, None, Box::new(child), spec(2, 1), 2, None).unwrap();
        s.stop();
        s.stop();
        drop(s);
        assert!(kills.load(Ordering::SeqCst) >= 1);
        wait_until("the worker to end", || closed.load(Ordering::Acquire));
    }

    #[test]
    fn a_cancel_flag_ends_the_stream_as_cancelled() {
        struct Endless;
        impl Read for Endless {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                std::thread::sleep(Duration::from_millis(1));
                buf.fill(1);
                Ok(buf.len())
            }
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let mut s = FrameStream::from_parts(Box::new(Endless), None, Box::new(NoKill), spec(2, 1), 2, Some(cancel.clone())).unwrap();
        assert!(matches!(s.wait(Duration::from_secs(10)), Poll::Frame(_)));
        cancel.store(true, Ordering::Release);
        let end = loop {
            match s.wait(Duration::from_secs(10)) {
                Poll::Ended(r) => break r,
                Poll::Frame(_) | Poll::Empty => {}
            }
        };
        assert!(matches!(end, Err(VideoError::Cancelled)), "{end:?}");
    }

    #[test]
    fn a_child_that_wrote_no_frame_says_why_or_says_there_was_none() {
        let say = |stderr: &str| {
            let err: Option<Box<dyn Read + Send>> = Some(Box::new(Cursor::new(stderr.as_bytes().to_vec())));
            let mut s = FrameStream::from_parts(Box::new(Cursor::new(Vec::new())), err, Box::new(NoKill), spec(2, 1), 2, None).unwrap();
            loop {
                match s.wait(Duration::from_secs(10)) {
                    Poll::Ended(r) => break r,
                    Poll::Frame(_) | Poll::Empty => {}
                }
            }
        };
        match say("Invalid data found when processing input\n") {
            Err(VideoError::Failed(text)) => assert!(text.contains("Invalid data"), "{text}"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(say(""), Err(VideoError::NoFrame)));
    }

    #[test]
    fn the_error_tail_is_bounded() {
        let tail = Mutex::new(Vec::new());
        drain_tail(Cursor::new(vec![b'x'; MAX_STDERR * 3 + 5]), &tail);
        assert_eq!(tail.lock().unwrap().len(), MAX_STDERR);
    }

    #[test]
    fn a_stream_of_no_size_is_refused_and_its_child_killed() {
        let (pipe, child, kills, _closed, _writer) = mock();
        let err = FrameStream::from_parts(pipe, None, Box::new(child), spec(0, 5), 2, None).err();
        assert!(matches!(err, Some(VideoError::Invalid(_))), "{err:?}");
        assert_eq!(kills.load(Ordering::SeqCst), 1);
        let huge = spec(u32::MAX, u32::MAX);
        assert!(huge.frame_len().is_none(), "no overflow, no giant buffer");
        assert_eq!(spec(1920, 1080).frame_len(), Some(1920 * 1080 * 4));
        let bad_rate = StreamSpec { fps: 0.0, ..spec(2, 1) };
        let err = FrameStream::from_parts(Box::new(Cursor::new(Vec::new())), None, Box::new(NoKill), bad_rate, 2, None).err();
        assert!(matches!(err, Some(VideoError::Invalid(_))));
    }

    #[test]
    fn the_stream_rate_is_the_clips_held_to_the_cap() {
        assert_eq!(playback_fps(Some(24.0), None), 24.0);
        assert_eq!(playback_fps(Some(60.0), Some(30.0)), 30.0);
        assert_eq!(playback_fps(Some(24.0), Some(30.0)), 24.0, "a cap never raises the rate");
        assert_eq!(playback_fps(None, None), FALLBACK_FPS);
        assert_eq!(playback_fps(Some(f64::NAN), Some(-1.0)), FALLBACK_FPS);
        assert_eq!(playback_fps(Some(1000.0), None), MAX_FPS);
        assert_eq!(playback_fps(Some(0.2), None), 1.0);
    }
}
