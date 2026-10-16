//! Map tiles (#114): fetching the map's raster tiles over the network, only when the caller says the user turned
//! the online map on, and caching them on disk. The tile server's usage policy
//! (<https://operations.osmfoundation.org/policies/tiles/>) is kept here:
//!
//! - **Privacy first:** [`TileLoader::want`] with `online` false returns [`TileState::Offline`] and does nothing
//!   else: no thread, no socket, no cache read. The tiles a map shows reveal the area the user is looking at.
//! - **A User-Agent that names Ember** and where its makers can be reached ([`crate::map::user_agent`]).
//! - **A cache:** a tile is fetched once and kept on disk for a week ([`FRESH_FOR`]), under a size cap
//!   ([`CACHE_CAP`]); the oldest tiles go first. A stale tile is shown when the server cannot be reached.
//! - **No bulk download:** only the tiles the map shows are asked for, nearest the centre first; the ones a
//!   moved map no longer shows are dropped from the queue; at most [`MAX_QUEUE`] wait and [`MAX_WORKERS`] (two)
//!   connections run at once.
//! - **A stop when refused:** an HTTP 403 or 429 pauses every request for [`BLOCKED_FOR`]; cancelling the
//!   activity row pauses for [`RETRY_AFTER`].
//! - **Attribution** is drawn by the map ([`crate::map::attribution`]).
//!
//! A load of [`LARGE_BATCH`] tiles or more is one row in the activity stack ("Loading map tiles", cancellable);
//! a few tiles are not worth a row. The transfer is [`lightcraft_fetch::http`] (pure-Rust HTTPS) on two
//! background threads; the UI thread only asks and draws what is ready. Native only: on wasm32 the map has no
//! tiles.
//!
//! Sources: own design; policy text from the OpenStreetMap Foundation's Tile Usage Policy.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use lightcraft_fetch::http::{self, HttpError, Limits, Url};

use crate::activity::{Activity, Cancel, TaskGuard, Unit};
use crate::map::{TileKey, check_template, tile_url, user_agent};

/// The largest the tile folder is allowed to grow before the oldest tiles are deleted (bytes).
pub const CACHE_CAP: u64 = 256 * 1024 * 1024;
/// How long a cached tile is used without asking the server again.
pub const FRESH_FOR: Duration = Duration::from_secs(7 * 24 * 3600);
/// Connections to the tile server at once (the policy allows two).
pub const MAX_WORKERS: usize = 2;
/// Tiles waiting for a connection; the map never shows more than this many at once.
pub const MAX_QUEUE: usize = 64;
/// From this many tiles in flight or waiting the load is shown in the activity stack.
pub const LARGE_BATCH: usize = 12;
/// How long a failed tile waits before it is asked for again; also how long cancelling pauses.
pub const RETRY_AFTER: Duration = Duration::from_secs(60);
/// How long every request pauses after the server refuses us (HTTP 403 or 429).
pub const BLOCKED_FOR: Duration = Duration::from_secs(600);
/// The largest tile accepted (bytes), and the most pixels in one.
const MAX_TILE_BYTES: usize = 1 << 20;
const MAX_TILE_PIXELS: u64 = 1 << 20;
/// Decoded tiles kept in memory.
const MEMORY_TILES: usize = 512;
/// Files written between two checks of the cache size.
const WRITES_PER_TRIM: u32 = 50;

/// A decoded tile: 8-bit sRGB RGBA, row by row.
#[derive(Debug)]
pub struct TileImage {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// What a tile is now.
#[derive(Clone, Debug)]
pub enum TileState {
    /// The online map is off (or the tile address is not usable): nothing was asked for.
    Offline,
    /// Waiting for, or being fetched from, the server.
    Loading,
    /// The server could not give it (or has paused us); asked again in a minute.
    Failed,
    Ready(Arc<TileImage>),
}

/// Why a fetch ended without a tile.
#[derive(Debug, PartialEq, Eq)]
pub enum FetchError {
    /// The server refuses us (HTTP 403 or 429): stop asking for a while.
    Refused(String),
    Failed(String),
    Cancelled,
}

/// Gets one tile's bytes. The real one is [`HttpFetcher`]; tests bring their own.
pub trait Fetcher: Send + Sync {
    fn get(&self, url: &str, cancel: &AtomicBool) -> Result<Vec<u8>, FetchError>;
}

/// HTTPS (or HTTP, for a local server) with Ember's User-Agent.
pub struct HttpFetcher;

fn from_http(e: HttpError) -> FetchError {
    if e == HttpError::Cancelled { FetchError::Cancelled } else { FetchError::Failed(e.to_string()) }
}

impl Fetcher for HttpFetcher {
    fn get(&self, url: &str, cancel: &AtomicBool) -> Result<Vec<u8>, FetchError> {
        let mut current = Url::parse(url).map_err(from_http)?;
        let limits = Limits {
            connect: Duration::from_secs(10),
            stall: Duration::from_secs(15),
            cancel,
            deadline: Some(Instant::now() + Duration::from_secs(40)),
        };
        let headers = [("User-Agent", user_agent())];
        for _ in 0..4 {
            let mut response = http::get(&current, &headers, &limits).map_err(from_http)?;
            match response.status {
                200 => {}
                301 | 302 | 307 | 308 => {
                    let location = response.header("location").ok_or_else(|| FetchError::Failed("a redirect without a location".into()))?;
                    let next = current.join(location).map_err(from_http)?;
                    if current.tls && !next.tls {
                        return Err(FetchError::Failed("a redirect from https to plain http".into()));
                    }
                    current = next;
                    continue;
                }
                403 | 429 => return Err(FetchError::Refused(format!("HTTP {}", response.status))),
                status => return Err(FetchError::Failed(format!("HTTP {status}"))),
            }
            if response.content_length().is_some_and(|n| n > MAX_TILE_BYTES as u64) {
                return Err(FetchError::Failed("the tile is larger than a map tile can be".into()));
            }
            let mut body = Vec::new();
            let mut buf = [0u8; 16 * 1024];
            loop {
                let n = response.read(&mut buf, &limits).map_err(from_http)?;
                if n == 0 {
                    return Ok(body);
                }
                body.extend_from_slice(buf.get(..n).unwrap_or_default());
                if body.len() > MAX_TILE_BYTES {
                    return Err(FetchError::Failed("the tile is larger than a map tile can be".into()));
                }
            }
        }
        Err(FetchError::Failed("too many redirects".into()))
    }
}

struct Job {
    key: TileKey,
    url: String,
    path: Option<PathBuf>,
}

/// A load big enough to show: its activity row and how many tiles it has finished.
struct Batch {
    task: TaskGuard,
    done: u64,
}

struct State {
    /// The tile address the tiles below belong to.
    template: String,
    /// Changes with the template, so a tile that arrives for the old one is dropped.
    generation: u64,
    ready: HashMap<TileKey, Arc<TileImage>>,
    /// Oldest first, for dropping from memory.
    order: VecDeque<TileKey>,
    queue: VecDeque<Job>,
    running: HashSet<TileKey>,
    failed: HashMap<TileKey, Instant>,
    workers: usize,
    /// No request before this time (the server refused us, or the user cancelled).
    paused_until: Option<Instant>,
    batch: Option<Batch>,
    /// Raised by the activity row's cross; replaced when it has been handled.
    cancel: Arc<AtomicBool>,
    writes: u32,
    trimmed: bool,
}

impl State {
    fn pending(&self) -> usize {
        self.queue.len() + self.running.len()
    }

    fn paused(&self, now: Instant) -> bool {
        self.paused_until.is_some_and(|t| now < t)
    }

    /// The user cancelled: drop what waits and pause.
    fn handle_cancel(&mut self, now: Instant) {
        if self.cancel.load(Ordering::Relaxed) {
            self.queue.clear();
            self.paused_until = Some(now + RETRY_AFTER);
            self.cancel = Arc::new(AtomicBool::new(false));
        }
    }

    fn remember(&mut self, key: TileKey, image: Arc<TileImage>) {
        if self.ready.insert(key, image).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > MEMORY_TILES {
            if let Some(old) = self.order.pop_front() {
                self.ready.remove(&old);
            }
        }
    }
}

struct Shared {
    fetcher: Arc<dyn Fetcher>,
    dir: Option<PathBuf>,
    cap: u64,
    activity: Activity,
    state: Mutex<State>,
    /// Requests sent to the fetcher so far.
    fetched: AtomicUsize,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Start, update or end the activity row to match the work left.
    fn sync_batch(&self, st: &mut State) {
        let pending = st.pending();
        if pending == 0 {
            st.batch = None;
            return;
        }
        if st.batch.is_none() && pending >= LARGE_BATCH {
            let task = self.activity.start("map", "Loading map tiles", Cancel::Flag(st.cancel.clone()));
            task.set_unit(Unit::Count);
            st.batch = Some(Batch { task, done: 0 });
        }
        if let Some(b) = &st.batch {
            b.task.progress(b.done, b.done + pending as u64);
        }
    }
}

/// Loads, caches and hands out map tiles. Cheap to clone; the clones share their tiles and workers.
#[derive(Clone)]
pub struct TileLoader {
    shared: Arc<Shared>,
}

impl TileLoader {
    /// A loader that gets tiles from `fetcher` and keeps them under `dir` (none: in memory only), at most `cap` bytes.
    pub fn new(fetcher: Arc<dyn Fetcher>, dir: Option<PathBuf>, cap: u64, activity: Activity) -> TileLoader {
        let state = State {
            template: String::new(),
            generation: 0,
            ready: HashMap::new(),
            order: VecDeque::new(),
            queue: VecDeque::new(),
            running: HashSet::new(),
            failed: HashMap::new(),
            workers: 0,
            paused_until: None,
            batch: None,
            cancel: Arc::new(AtomicBool::new(false)),
            writes: 0,
            trimmed: false,
        };
        TileLoader { shared: Arc::new(Shared { fetcher, dir, cap, activity, state: Mutex::new(state), fetched: AtomicUsize::new(0) }) }
    }

    /// The real loader: HTTPS tiles cached in the settings folder (`map-tiles`), shown in `activity` when large.
    pub fn http(activity: Activity) -> TileLoader {
        TileLoader::new(Arc::new(HttpFetcher), crate::config::config_dir().map(|d| d.join("map-tiles")), CACHE_CAP, activity)
    }

    /// Requests sent to the tile server so far.
    pub fn fetches(&self) -> usize {
        self.shared.fetched.load(Ordering::Relaxed)
    }

    /// Tiles waiting or being fetched.
    pub fn pending(&self) -> usize {
        self.shared.lock().pending()
    }

    /// Cancel the load in progress (what the activity row's cross does): waiting tiles are dropped and requests pause for a minute.
    pub fn cancel(&self) {
        let mut st = self.shared.lock();
        st.cancel.store(true, Ordering::Relaxed);
        st.handle_cancel(Instant::now());
    }

    /// One call per frame with the tiles the map shows now (nearest the centre first): where each is, and asks for
    /// the ones that are missing. With `online` false, or a `template` that is not a usable address, nothing at
    /// all happens: every tile is [`TileState::Offline`].
    pub fn want(&self, online: bool, template: &str, keys: &[TileKey]) -> Vec<TileState> {
        if !online || check_template(template).is_err() {
            return vec![TileState::Offline; keys.len()];
        }
        let now = Instant::now();
        let mut st = self.shared.lock();
        st.handle_cancel(now);
        if st.template != template {
            // another tile source: nothing of the old one is shown or finished
            st.template = template.to_string();
            st.generation += 1;
            st.ready.clear();
            st.order.clear();
            st.failed.clear();
            st.queue.clear();
        }
        if st.paused_until.is_some_and(|t| now >= t) {
            st.paused_until = None;
        }
        let paused = st.paused(now);
        // the map moved on: tiles it no longer shows are dropped from the queue (those on the wire finish)
        let wanted: HashSet<TileKey> = keys.iter().copied().collect();
        st.queue.retain(|j| wanted.contains(&j.key));
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(image) = st.ready.get(key) {
                out.push(TileState::Ready(image.clone()));
            } else if st.running.contains(key) || st.queue.iter().any(|j| j.key == *key) {
                out.push(TileState::Loading);
            } else if paused || st.failed.get(key).is_some_and(|t| now.duration_since(*t) < RETRY_AFTER) {
                out.push(TileState::Failed);
            } else if st.queue.len() >= MAX_QUEUE {
                out.push(TileState::Loading);
            } else {
                let path = self.shared.dir.as_deref().map(|d| cache_path(d, template, *key));
                st.queue.push_back(Job { key: *key, url: tile_url(template, *key), path });
                out.push(TileState::Loading);
            }
        }
        self.spawn_workers(&mut st);
        self.shared.sync_batch(&mut st);
        out
    }

    fn spawn_workers(&self, st: &mut State) {
        while st.workers < MAX_WORKERS && st.workers < st.queue.len() {
            let shared = self.shared.clone();
            match std::thread::Builder::new().name("map-tiles".into()).spawn(move || worker(&shared)) {
                Ok(_) => st.workers += 1,
                Err(e) => {
                    log::warn!("map tiles: could not start a worker: {e}");
                    break;
                }
            }
        }
    }
}

/// What a job came to.
enum Outcome {
    Ready(Arc<TileImage>),
    Failed,
    Refused,
    Cancelled,
}

fn worker(shared: &Arc<Shared>) {
    loop {
        let (job, cancel, generation) = {
            let mut st = shared.lock();
            st.handle_cancel(Instant::now());
            let Some(job) = st.queue.pop_front() else {
                st.workers = st.workers.saturating_sub(1);
                shared.sync_batch(&mut st);
                return;
            };
            st.running.insert(job.key);
            let first = !std::mem::replace(&mut st.trimmed, true);
            let (cancel, generation) = (st.cancel.clone(), st.generation);
            drop(st);
            if first && let Some(dir) = &shared.dir {
                trim_cache(dir, shared.cap);
            }
            (job, cancel, generation)
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| process(shared, &job, &cancel))).unwrap_or(Outcome::Failed);
        let mut trim = false;
        {
            let mut st = shared.lock();
            st.running.remove(&job.key);
            if st.generation == generation {
                match outcome {
                    Outcome::Ready(image) => {
                        st.failed.remove(&job.key);
                        st.remember(job.key, image);
                    }
                    Outcome::Failed => {
                        st.failed.insert(job.key, Instant::now());
                    }
                    Outcome::Refused => {
                        st.failed.insert(job.key, Instant::now());
                        // every request stops, the waiting ones too
                        st.queue.clear();
                        st.paused_until = Some(Instant::now() + BLOCKED_FOR);
                        log::warn!("map tiles: the tile server refused the request; paused for {} minutes", BLOCKED_FOR.as_secs() / 60);
                    }
                    Outcome::Cancelled => {}
                }
            }
            if let Some(b) = st.batch.as_mut() {
                b.done += 1;
            }
            if job.path.is_some() {
                st.writes += 1;
                trim = st.writes.is_multiple_of(WRITES_PER_TRIM);
            }
            shared.sync_batch(&mut st);
        }
        if trim && let Some(dir) = &shared.dir {
            trim_cache(dir, shared.cap);
        }
    }
}

/// The disk copy if there is one, else the server's.
fn process(shared: &Shared, job: &Job, cancel: &AtomicBool) -> Outcome {
    if cancel.load(Ordering::Relaxed) {
        return Outcome::Cancelled;
    }
    let mut stale = None;
    if let Some(path) = &job.path
        && let Some((bytes, age)) = read_cached(path)
    {
        match decode(&bytes) {
            Some(image) if age < FRESH_FOR => return Outcome::Ready(image),
            Some(image) => stale = Some(image),
            None => {
                let _ = fs::remove_file(path);
            }
        }
    }
    let give_up = |stale: Option<Arc<TileImage>>, outcome: Outcome| stale.map_or(outcome, Outcome::Ready);
    if shared.lock().paused(Instant::now()) {
        return give_up(stale, Outcome::Failed);
    }
    shared.fetched.fetch_add(1, Ordering::Relaxed);
    match shared.fetcher.get(&job.url, cancel) {
        Ok(bytes) => match decode(&bytes) {
            Some(image) => {
                if let Some(path) = &job.path {
                    write_cached(path, &bytes);
                }
                Outcome::Ready(image)
            }
            None => give_up(stale, Outcome::Failed),
        },
        Err(FetchError::Cancelled) => Outcome::Cancelled,
        Err(FetchError::Refused(_)) => give_up(stale, Outcome::Refused),
        Err(FetchError::Failed(why)) => {
            log::debug!("map tiles: {}: {why}", job.url);
            give_up(stale, Outcome::Failed)
        }
    }
}

/// A PNG or JPEG tile as RGBA; anything else (an error page, a huge image) is not a tile.
fn decode(bytes: &[u8]) -> Option<Arc<TileImage>> {
    if bytes.is_empty() || bytes.len() > MAX_TILE_BYTES {
        return None;
    }
    let options = lightcraft_codecs::DecodeOptions { max_size: None, max_pixels: MAX_TILE_PIXELS };
    let rgba = lightcraft_codecs::decode(bytes, options).ok()?.to_srgb8();
    if rgba.width < 16 || rgba.height < 16 || rgba.width > 1024 || rgba.height > 1024 {
        return None;
    }
    Some(Arc::new(TileImage { width: rgba.width, height: rgba.height, rgba: rgba.as_bytes() }))
}

/// 64-bit FNV-1a: names the folder of a tile source.
fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// Where a tile is kept: one folder per tile source, then `z/x/y`.
pub fn cache_path(dir: &Path, template: &str, key: TileKey) -> PathBuf {
    dir.join(format!("{:016x}", fnv(template))).join(key.z.to_string()).join(key.x.to_string()).join(format!("{}.tile", key.y))
}

fn read_cached(path: &Path) -> Option<(Vec<u8>, Duration)> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_TILE_BYTES as u64 {
        return None;
    }
    let age = meta.modified().ok().and_then(|m| SystemTime::now().duration_since(m).ok()).unwrap_or_default();
    Some((fs::read(path).ok()?, age))
}

/// Written beside its place and renamed, so a half-written tile is never read. A full disk loses the cache, not the map.
fn write_cached(path: &Path, bytes: &[u8]) {
    let Some(parent) = path.parent() else { return };
    let written = fs::create_dir_all(parent).and_then(|()| lightcraft_catalog::safe_file::write_atomic_nosync(path, bytes));
    if let Err(e) = written {
        log::debug!("map tiles: cannot cache {}: {e}", path.display());
    }
}

/// Delete the oldest files under `dir` until it holds at most 90 % of `cap` bytes; nothing when it is already within
/// `cap`. Returns the bytes deleted. Looks four folders deep (`source/z/x/y`).
pub fn trim_cache(dir: &Path, cap: u64) -> u64 {
    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((folder, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&folder) else { continue };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                if depth < 4 {
                    stack.push((entry.path(), depth + 1));
                }
            } else if meta.is_file() {
                files.push((entry.path(), meta.len(), meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)));
            }
        }
    }
    let total: u64 = files.iter().map(|f| f.1).sum();
    if total <= cap {
        return 0;
    }
    files.sort_by_key(|f| f.2);
    let target = cap / 10 * 9;
    let (mut left, mut freed) = (total, 0);
    for (path, len, _) in files {
        if left <= target {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            left = left.saturating_sub(len);
            freed += len;
        }
    }
    freed
}

#[cfg(test)]
mod tests {
    use std::sync::Condvar;

    use super::*;
    use crate::map::DEFAULT_TILE_URL;

    /// A 256 × 256 PNG.
    fn png() -> Vec<u8> {
        let data: Vec<u8> = (0..256 * 256).flat_map(|i| [(i % 256) as u8, 120, 200, 255]).collect();
        let image = lightcraft_codecs::EncodeImage { width: 256, height: 256, channels: 4, samples: lightcraft_codecs::Samples::U8(&data) };
        lightcraft_codecs::encode_png(&image, &lightcraft_codecs::EncodeMeta::default()).unwrap()
    }

    /// Serves `png()` for every address, counting requests and how many run at once; can hold requests until opened.
    struct Fake {
        png: Vec<u8>,
        urls: Mutex<Vec<String>>,
        in_flight: AtomicUsize,
        most: AtomicUsize,
        mode: Mutex<Mode>,
        gate: (Mutex<bool>, Condvar),
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Serve,
        Refuse,
    }

    impl Fake {
        fn new(open: bool) -> Arc<Fake> {
            Arc::new(Fake {
                png: png(),
                urls: Mutex::new(Vec::new()),
                in_flight: AtomicUsize::new(0),
                most: AtomicUsize::new(0),
                mode: Mutex::new(Mode::Serve),
                gate: (Mutex::new(open), Condvar::new()),
            })
        }

        fn open(&self) {
            *self.gate.0.lock().unwrap() = true;
            self.gate.1.notify_all();
        }

        fn calls(&self) -> usize {
            self.urls.lock().unwrap().len()
        }
    }

    impl Fetcher for Fake {
        fn get(&self, url: &str, cancel: &AtomicBool) -> Result<Vec<u8>, FetchError> {
            self.urls.lock().unwrap().push(url.to_string());
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.most.fetch_max(now, Ordering::SeqCst);
            let mut open = self.gate.0.lock().unwrap();
            while !*open {
                if cancel.load(Ordering::Relaxed) {
                    self.in_flight.fetch_sub(1, Ordering::SeqCst);
                    return Err(FetchError::Cancelled);
                }
                open = self.gate.1.wait_timeout(open, Duration::from_millis(5)).unwrap().0;
            }
            drop(open);
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            match *self.mode.lock().unwrap() {
                Mode::Serve => Ok(self.png.clone()),
                Mode::Refuse => Err(FetchError::Refused("HTTP 429".into())),
            }
        }
    }

    fn folder(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ember-map-tiles-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn keys(n: u32) -> Vec<TileKey> {
        (0..n).map(|x| TileKey { z: 8, x, y: 5 }).collect()
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn all_ready(loader: &TileLoader, k: &[TileKey]) -> bool {
        loader.want(true, DEFAULT_TILE_URL, k).iter().all(|s| matches!(s, TileState::Ready(_)))
    }

    #[test]
    fn nothing_is_fetched_cached_or_started_while_online_is_off() {
        let fake = Fake::new(true);
        let dir = folder("offline");
        let activity = Activity::default();
        let loader = TileLoader::new(fake.clone(), Some(dir.clone()), CACHE_CAP, activity.clone());
        for _ in 0..3 {
            let states = loader.want(false, DEFAULT_TILE_URL, &keys(30));
            assert_eq!(states.len(), 30);
            assert!(states.iter().all(|s| matches!(s, TileState::Offline)));
        }
        // a tile address that is not one is the same as offline
        assert!(loader.want(true, "https://x.test/{z}/{x}.png", &keys(3)).iter().all(|s| matches!(s, TileState::Offline)));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!((fake.calls(), loader.fetches(), loader.pending()), (0, 0, 0), "no request was made");
        assert!(activity.list().is_empty() && !dir.exists(), "no activity row, no cache folder");
    }

    #[test]
    fn a_tile_is_fetched_once_kept_on_disk_and_served_after_a_restart() {
        let fake = Fake::new(true);
        let dir = folder("cache");
        let loader = TileLoader::new(fake.clone(), Some(dir.clone()), CACHE_CAP, Activity::default());
        let k = keys(2);
        wait_for("the tiles", || all_ready(&loader, &k));
        assert_eq!(fake.calls(), 2);
        let urls = fake.urls.lock().unwrap().clone();
        assert!(urls.contains(&"https://tile.openstreetmap.org/8/0/5.png".to_string()), "{urls:?}");
        let TileState::Ready(tile) = loader.want(true, DEFAULT_TILE_URL, &k[..1])[0].clone() else { panic!("ready") };
        assert_eq!((tile.width, tile.height, tile.rgba.len()), (256, 256, 256 * 256 * 4));
        assert!(tile.rgba[..4].iter().zip([0u8, 120, 200, 255]).all(|(a, b)| a.abs_diff(b) <= 1), "{:?}", &tile.rgba[..4]);
        // the same tile again is memory; a new loader reads the disk copy, not the server
        loader.want(true, DEFAULT_TILE_URL, &k);
        assert_eq!(fake.calls(), 2);
        let second = Fake::new(true);
        let restarted = TileLoader::new(second.clone(), Some(dir.clone()), CACHE_CAP, Activity::default());
        wait_for("the cached tiles", || all_ready(&restarted, &k));
        assert_eq!((second.calls(), restarted.fetches()), (0, 0));
        // another tile source is another folder and another request
        let other = "https://tiles.example/{z}/{x}/{y}.png";
        wait_for("the other source", || restarted.want(true, other, &k[..1]).iter().all(|s| matches!(s, TileState::Ready(_))));
        assert_eq!(second.calls(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_week_old_tile_is_asked_for_again_and_kept_when_the_server_refuses() {
        let dir = folder("stale");
        let key = keys(1)[0];
        let path = cache_path(&dir, DEFAULT_TILE_URL, key);
        write_cached(&path, &png());
        let old = SystemTime::now() - FRESH_FOR - Duration::from_secs(3600);
        fs::File::options().write(true).open(&path).unwrap().set_modified(old).unwrap();
        let fake = Fake::new(true);
        *fake.mode.lock().unwrap() = Mode::Refuse;
        let loader = TileLoader::new(fake.clone(), Some(dir.clone()), CACHE_CAP, Activity::default());
        wait_for("the stale tile", || all_ready(&loader, &[key]));
        assert_eq!(fake.calls(), 1, "a week-old tile is asked for again");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_large_load_is_one_activity_row_two_connections_at_most_and_a_small_one_has_none() {
        let fake = Fake::new(false);
        let activity = Activity::default();
        let loader = TileLoader::new(fake.clone(), None, CACHE_CAP, activity.clone());
        let few = keys(3);
        loader.want(true, DEFAULT_TILE_URL, &few);
        assert!(activity.list().is_empty(), "three tiles are not worth an activity row");
        fake.open();
        wait_for("the few tiles", || all_ready(&loader, &few));
        let slow = Fake::new(false);
        let loader = TileLoader::new(slow.clone(), None, CACHE_CAP, activity.clone());
        let many = keys(20);
        loader.want(true, DEFAULT_TILE_URL, &many);
        wait_for("both connections", || slow.in_flight.load(Ordering::SeqCst) == MAX_WORKERS);
        loader.want(true, DEFAULT_TILE_URL, &many);
        let rows = activity.list();
        assert_eq!(rows.len(), 1, "one row for the whole load, however many tiles");
        assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].total, rows[0].cancellable), ("map", "Loading map tiles", 20, true));
        slow.open();
        wait_for("the tiles", || all_ready(&loader, &many));
        wait_for("the row to end", || activity.list().is_empty());
        assert_eq!(slow.most.load(Ordering::SeqCst), MAX_WORKERS, "never more than two connections");
        assert_eq!(slow.calls(), 20);
    }

    #[test]
    fn tiles_the_map_no_longer_shows_are_dropped_not_fetched() {
        let fake = Fake::new(false);
        let loader = TileLoader::new(fake.clone(), None, CACHE_CAP, Activity::default());
        let many = keys(20);
        loader.want(true, DEFAULT_TILE_URL, &many);
        wait_for("both connections", || fake.in_flight.load(Ordering::SeqCst) == MAX_WORKERS);
        // the map moved: only one of the waiting tiles is still on screen
        let states = loader.want(true, DEFAULT_TILE_URL, &many[10..11]);
        assert!(matches!(states[0], TileState::Loading));
        assert_eq!(loader.pending(), MAX_WORKERS + 1, "the two on the wire and the one still wanted");
        fake.open();
        wait_for("the tile", || all_ready(&loader, &many[10..11]));
        wait_for("the queue to empty", || loader.pending() == 0);
        assert_eq!(fake.calls(), MAX_WORKERS + 1, "the 17 dropped tiles were never requested");
    }

    #[test]
    fn a_refusing_server_stops_every_request_until_the_pause_ends() {
        let fake = Fake::new(true);
        *fake.mode.lock().unwrap() = Mode::Refuse;
        let loader = TileLoader::new(fake.clone(), None, CACHE_CAP, Activity::default());
        let k = keys(10);
        loader.want(true, DEFAULT_TILE_URL, &k);
        wait_for("the refusal", || loader.pending() == 0);
        let sent = fake.calls();
        assert!((1..=MAX_WORKERS).contains(&sent), "only the requests already on their way: {sent}");
        let states = loader.want(true, DEFAULT_TILE_URL, &k);
        assert!(states.iter().all(|s| matches!(s, TileState::Failed)), "every tile reads as failed while paused");
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(fake.calls(), sent, "nothing more is sent");
    }

    #[test]
    fn cancelling_the_row_drops_the_queue_and_pauses() {
        let fake = Fake::new(false);
        let activity = Activity::default();
        let loader = TileLoader::new(fake.clone(), None, CACHE_CAP, activity.clone());
        let k = keys(20);
        loader.want(true, DEFAULT_TILE_URL, &k);
        wait_for("both connections", || fake.in_flight.load(Ordering::SeqCst) == MAX_WORKERS);
        let id = activity.list()[0].id;
        activity.cancel(id).unwrap();
        wait_for("the load to stop", || loader.want(true, DEFAULT_TILE_URL, &k).iter().all(|s| matches!(s, TileState::Failed)));
        wait_for("the row to end", || activity.list().is_empty());
        assert_eq!(fake.calls(), MAX_WORKERS, "the waiting tiles were never asked for");
    }

    #[test]
    fn an_error_page_is_not_a_tile() {
        assert!(decode(b"<html>429 Too Many Requests</html>").is_none());
        assert!(decode(&[]).is_none());
        assert!(decode(&png()).is_some());
        let mut cut = png();
        cut.truncate(60);
        assert!(decode(&cut).is_none());
    }

    #[test]
    fn the_cache_is_trimmed_to_its_cap_oldest_first() {
        let dir = folder("trim");
        let now = SystemTime::now();
        for (i, age) in [(0u32, 500u64), (1, 400), (2, 300), (3, 200), (4, 100)] {
            let path = cache_path(&dir, DEFAULT_TILE_URL, TileKey { z: 3, x: i, y: 1 });
            write_cached(&path, &[7u8; 1000]);
            fs::File::options().write(true).open(&path).unwrap().set_modified(now - Duration::from_secs(age)).unwrap();
        }
        assert_eq!(trim_cache(&dir, 5000), 0, "within the cap nothing goes");
        // over the cap: down to 90 % of it, the two oldest first
        let freed = trim_cache(&dir, 4000);
        assert_eq!(freed, 2000);
        let left = |i: u32| cache_path(&dir, DEFAULT_TILE_URL, TileKey { z: 3, x: i, y: 1 }).exists();
        assert_eq!([left(0), left(1), left(2), left(3), left(4)], [false, false, true, true, true]);
        assert_eq!(trim_cache(&dir.join("missing"), 0), 0, "a folder that is not there is nothing to trim");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tile_source_is_a_folder_of_its_own() {
        let d = Path::new("cache");
        let key = TileKey { z: 3, x: 4, y: 5 };
        assert_eq!(cache_path(d, "https://a.test/{z}/{x}/{y}.png", key), cache_path(d, "https://a.test/{z}/{x}/{y}.png", key));
        assert_ne!(cache_path(d, "https://a.test/{z}/{x}/{y}.png", key), cache_path(d, "https://b.test/{z}/{x}/{y}.png", key));
        assert!(cache_path(d, DEFAULT_TILE_URL, key).ends_with(Path::new("3").join("4").join("5.tile")));
    }
}
