use std::sync::Arc;

use lightcraft_raster::Rgba8;

use super::*;

#[test]
fn cleared_generation_cannot_read_or_repopulate_memory_or_disk() {
    let dir = std::env::temp_dir().join(format!("lc-thumbnail-generation-{}-{}", std::process::id(), next_tick()));
    let cache = Arc::new(PreviewCache::with_disk(1 << 20, &dir, 1 << 20));
    let key = hash_bytes(b"same-key");
    let old = cache.generation();
    let wrong = Arc::new(gradient(8, 8, 255));
    let correct = Arc::new(gradient(8, 8, 0));
    cache.put_at(old, key, wrong.clone());
    assert!(cache.get_at(old, key).is_some());
    cache.clear();
    assert!(cache.get(key).is_none());
    assert!(cache.get_at(old, key).is_none());
    cache.put_at(old, key, wrong.clone());
    cache.put_deferred_at(old, key, wrong);
    assert!(cache.get(key).is_none());
    assert!(cache.disk().unwrap().get(key).is_none());
    cache.put_at(cache.generation(), key, correct.clone());
    assert_eq!(cache.get(key).unwrap().as_bytes(), correct.as_bytes());
    cache.clear();
}

#[test]
fn retired_cache_keeps_its_files_but_refuses_every_read_and_write() {
    let dir = temp_dir("retire");
    let old = Arc::new(PreviewCache::with_disk(1 << 20, &dir, 1 << 20));
    let (kept, late) = (hash_bytes(b"kept"), hash_bytes(b"late"));
    let img = Arc::new(gradient(8, 8, 7));
    let generation = old.generation();
    old.put_at(generation, kept, img.clone());
    old.retire();
    assert_ne!(old.generation(), generation);
    assert!(old.get_at(generation, kept).is_none());
    assert!(old.get(kept).is_none());
    old.put_at(generation, late, img.clone());
    old.put_deferred_at(generation, late, img.clone());
    old.put(late, img.clone());
    old.put_deferred(late, img.clone());
    old.clear();
    // a replacement for the same directory still finds the valid file, and only that
    let new = PreviewCache::with_disk(1 << 20, &dir, 1 << 20);
    assert_eq!(new.get(kept).map(|i| (i.width, i.height)), Some((8, 8)), "the disk file stays (JPEG: lossy)");
    assert!(new.get(late).is_none());
    new.clear();
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-preview-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn gradient(w: usize, h: usize, seed: u8) -> Rgba8 {
    let data = (0..w * h).map(|i| [((i % w) * 255 / w) as u8, ((i / w) * 255 / h) as u8, seed, 255]).collect();
    Rgba8 { width: w, height: h, data }
}

#[test]
fn hashes_are_stable_and_distinct() {
    let a = hash_bytes(b"hello");
    assert_eq!(a, hash_bytes(b"hello"));
    assert_ne!(a, hash_bytes(b"hellp"));
    assert_eq!(Hash128::parse(&a.to_string()), Some(a));
    assert_ne!(Hasher128::new().str("ab").str("c").finish(), Hasher128::new().str("a").str("bc").finish());
    // streaming == one shot
    assert_eq!(Hasher128::new().update(b"hel").update(b"lo").finish(), a);
}

#[test]
fn lru_evicts_least_recent_by_cost() {
    let mut l = Lru::new(10);
    l.insert(1, "a", 4);
    l.insert(2, "b", 4);
    assert_eq!(l.get(&1), Some(&"a")); // 1 is now most recent
    l.insert(3, "c", 4); // over budget: evicts 2
    assert!(l.contains(&1) && !l.contains(&2) && l.contains(&3));
    assert_eq!(l.cost(), 8);
    l.insert(4, "huge", 50); // alone over budget: kept, everything else evicted
    assert_eq!((l.len(), l.cost()), (1, 50));
    l.insert(4, "small", 1);
    assert_eq!((l.len(), l.cost()), (1, 1));
    l.retain(|k| *k != 4);
    assert!(l.is_empty());
}

#[test]
fn disk_cache_roundtrip_and_prune() {
    let dir = temp_dir("disk");
    let d = DiskCache::new(&dir, 40_000);
    let img = gradient(160, 100, 7);
    let k = hash_bytes(b"k1");
    assert!(d.get(k).is_none());
    d.put(k, &img);
    let back = d.get(k).expect("hit");
    assert_eq!((back.width, back.height), (160, 100));
    let err: i64 = img.data.iter().zip(&back.data).map(|(a, b)| (a[0] as i64 - b[0] as i64).abs() + (a[1] as i64 - b[1] as i64).abs()).max().unwrap();
    assert!(err < 24, "JPEG error {err}");
    assert!(back.data.iter().all(|p| p[3] == 255));
    // fill past the budget: the oldest entries go, the newest stay
    let keys: Vec<_> = (0..40u8).map(|i| hash_bytes(&[i, 1])).collect();
    for (i, k) in keys.iter().enumerate() {
        d.put(*k, &gradient(160, 100, i as u8));
    }
    assert!(d.size() <= 40_000, "{}", d.size());
    assert!(d.get(*keys.last().unwrap()).is_some());
    assert!(d.get(keys[0]).is_none());
    // a corrupt file is a miss and gets removed
    let k2 = hash_bytes(b"corrupt");
    d.put(k2, &img);
    let hex = k2.to_string();
    let p = dir.join(&hex[..2]).join(format!("{hex}.jpg"));
    std::fs::write(&p, b"not a jpeg").unwrap();
    assert!(d.get(k2).is_none());
    assert!(!p.exists());
    d.clear();
    assert_eq!(d.size(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn disk_cache_moves_its_files_and_leaves_the_rest_of_the_folder() {
    let (a, b) = (temp_dir("move-from"), temp_dir("move-to"));
    std::fs::create_dir_all(&a).unwrap();
    let d = DiskCache::new(&a, 1 << 30);
    let k = hash_bytes(b"moved thumbnail");
    d.put(k, &gradient(16, 16, 3));
    std::fs::write(a.join("notes.txt"), b"not a cache file").unwrap();
    let (moved, failed) = d.move_to(&b);
    assert_eq!((moved, failed.len()), (1, 0));
    assert!(d.get(k).is_none());
    assert!(DiskCache::new(&b, 1 << 30).get(k).is_some());
    assert!(a.join("notes.txt").exists(), "a file that is not a cache file stays");
    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
}

#[test]
fn disk_cache_overwrite_accounts_for_replaced_jpeg_only_once() {
    let dir = temp_dir("overwrite");
    let cache = DiskCache::new(&dir, 1 << 20);
    let (a, b) = (hash_bytes(b"overwrite-a"), hash_bytes(b"overwrite-b"));
    let first = gradient(80, 60, 4);
    cache.put(a, &first);
    cache.put(b, &gradient(80, 60, 7));
    let initial = cache.size();

    // Re-rendering the same key used to double-count bytes although the
    // replacement occupied exactly the same pathname on disk.
    cache.put(a, &first);
    assert_eq!(cache.size(), initial);
    assert!(cache.get(b).is_some());

    // A replacement with a different encoded size must also update the total.
    cache.put(a, &gradient(120, 90, 9));
    let path = |key: Hash128| {
        let hex = key.to_string();
        dir.join(&hex[..2]).join(format!("{hex}.jpg"))
    };
    let actual = std::fs::metadata(path(a)).unwrap().len() + std::fs::metadata(path(b)).unwrap().len();
    assert_eq!(cache.size(), actual);
    cache.clear();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preview_cache_memory_then_disk() {
    let dir = temp_dir("pc");
    let k = hash_bytes(b"x");
    {
        let c = PreviewCache::with_disk(1 << 20, &dir, 1 << 24);
        c.put(k, Arc::new(gradient(64, 48, 1)));
        assert_eq!(c.mem_usage().0, 1);
        assert!(c.get(k).is_some());
    }
    // a new process: memory is empty, disk serves it
    let c = PreviewCache::with_disk(1 << 20, &dir, 1 << 24);
    assert_eq!(c.mem_usage().0, 0);
    assert_eq!(c.get(k).map(|i| i.width), Some(64));
    assert_eq!(c.mem_usage().0, 1);
    assert_eq!(c.disk().unwrap().hits.load(std::sync::atomic::Ordering::Relaxed), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pool_priorities_dedupe_and_drop() {
    let mut p: JobPool<u32, u32> = JobPool::new(1);
    // One worker, held busy by a gate job while the others queue up.
    let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let g = gate.clone();
    let _ = p.submit(
        999,
        0,
        1000,
        Box::new(move || {
            let (m, cv) = &*g;
            let mut open = m.lock().unwrap();
            while !*open {
                open = cv.wait(open).unwrap();
            }
            999
        }),
    );
    // wait until the worker took the gate job
    while p.queued() > 0 {
        std::thread::yield_now();
    }
    let _ = p.submit(1, 1, 5, Box::new(|| 1)); // off-screen thumb
    let _ = p.submit(2, 1, 10, Box::new(|| 2)); // visible thumb
    let _ = p.submit(3, 1, 100, Box::new(|| 3)); // loupe
    let _ = p.submit(2, 2, 10, Box::new(|| 22)); // newer request for slot 2 replaces the queued one
    let _ = p.submit(4, 1, 5, Box::new(|| 4));
    assert_eq!(p.queued(), 4);
    let dropped = p.reprioritize(|s, pr| if *s == 4 { None } else { Some(pr) });
    assert_eq!(dropped, vec![4]);
    {
        let (m, cv) = &*gate;
        *m.lock().unwrap() = true;
        cv.notify_all();
    }
    let mut order = Vec::new();
    while order.len() < 4 {
        if let Some(d) = p.try_recv() {
            order.push((d.slot, d.key, d.result));
        } else {
            std::thread::yield_now();
        }
    }
    assert_eq!(order, vec![(999, 0, 999), (3, 1, 3), (2, 2, 22), (1, 1, 1)]);
}

#[test]
fn pool_runs_inline() {
    // No workers: run_inline drains the queue on the caller's thread (the wasm path).
    let mut p: JobPool<u8, u8> = JobPool::new(0);
    let _ = p.submit(1, 0, 1, Box::new(|| 1));
    let _ = p.submit(2, 0, 9, Box::new(|| 2));
    assert_eq!(p.run_inline(1), 1);
    assert_eq!(p.try_recv().map(|d| d.result), Some(2));
    assert_eq!(p.run_inline(10), 1);
    assert_eq!(p.try_recv().map(|d| d.result), Some(1));
}

fn pool_with_a_running_job() -> (JobPool<u8, ()>, std::sync::mpsc::Sender<()>) {
    let mut pool: JobPool<u8, ()> = JobPool::new(1);
    let (started, has_started) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let _ = pool.submit(
        1,
        0,
        1,
        Box::new(move || {
            let _ = started.send(());
            let _ = released.recv_timeout(std::time::Duration::from_secs(60));
        }),
    );
    has_started.recv().unwrap();
    (pool, release)
}

#[test]
fn pool_shutdown_waits_bounded_and_drops_queued_jobs() {
    let (mut pool, release) = pool_with_a_running_job();
    assert_eq!(pool.live_workers(), 1);
    assert!(!pool.shutdown(std::time::Duration::from_millis(1)));
    assert_eq!(pool.live_workers(), 1);
    assert!(pool.submit(2, 0, 1, Box::new(|| panic!("queued job must not run"))).is_err());
    assert_eq!(pool.queued(), 0);
    drop(release);
    assert!(pool.shutdown(std::time::Duration::from_secs(1)));
    assert_eq!(pool.live_workers(), 0);
}

#[test]
fn pool_worker_panic_is_terminal_failure_without_losing_worker() {
    let mut pool: JobPool<u8, ()> = JobPool::new(1);
    let _ = pool.submit(7, 42, 1, Box::new(|| panic!("synthetic worker failure")));
    let failure = loop {
        if let Some(failure) = pool.try_failure() {
            break failure;
        }
        std::thread::yield_now();
    };
    assert_eq!((failure.slot, failure.key), (7, 42));
    assert!(failure.error.contains("synthetic worker failure"));
    assert_eq!(pool.live_workers(), 1);
    assert!(pool.shutdown(std::time::Duration::from_secs(1)));
}

#[test]
fn pool_failure_identity_distinguishes_reused_slot_and_key() {
    let mut pool: JobPool<u8, u8> = JobPool::new(1);
    let (started, ready) = std::sync::mpsc::channel();
    let first = pool
        .submit(
            7,
            42,
            1,
            Box::new(move || {
                let _ = started.send(());
                panic!("stale failure");
            }),
        )
        .unwrap();
    ready.recv().unwrap();
    let second = pool.submit(7, 42, 1, Box::new(|| 9)).unwrap();
    let failure = loop {
        if let Some(failure) = pool.try_failure() {
            break failure;
        }
        std::thread::yield_now();
    };
    assert_eq!(failure.submission, first);
    assert_ne!(failure.submission, second);
    assert!(pool.shutdown(std::time::Duration::from_secs(1)));
}

#[test]
fn pool_drop_waits_for_running_job_without_joining_self() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let finished = Arc::new(AtomicBool::new(false));
    let marker = finished.clone();
    let mut pool: JobPool<u8, ()> = JobPool::new(1);
    let (started, has_started) = std::sync::mpsc::channel();
    let _ = pool.submit(
        1,
        0,
        1,
        Box::new(move || {
            let _ = started.send(());
            std::thread::sleep(std::time::Duration::from_millis(10));
            marker.store(true, Ordering::SeqCst);
        }),
    );
    has_started.recv().unwrap();
    drop(pool);
    assert!(finished.load(Ordering::SeqCst));
}

#[test]
fn pool_drop_retries_after_timed_shutdown() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let finished = Arc::new(AtomicBool::new(false));
    let marker = finished.clone();
    let mut pool: JobPool<u8, ()> = JobPool::new(1);
    let (started, has_started) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let _ = pool.submit(
        1,
        0,
        1,
        Box::new(move || {
            let _ = started.send(());
            let _ = released.recv();
            std::thread::sleep(std::time::Duration::from_millis(100));
            marker.store(true, Ordering::SeqCst);
        }),
    );
    has_started.recv().unwrap();
    assert!(!pool.shutdown(std::time::Duration::from_millis(1)));
    drop(release);
    drop(pool);
    assert!(finished.load(Ordering::SeqCst));
}

#[test]
fn disk_cache_never_touches_foreign_files() {
    // issue #98: a library opened on a folder that already has a `thumbs/` folder
    let dir = temp_dir("foreign");
    let foreign = [
        dir.join("readme.txt"),
        dir.join("ab").join("holiday.jpg"),
        dir.join("ab").join("0123456789abcdef0123456789abcdef.jpg"), // wrong shard
        dir.join("small").join("0123456789abcdef0123456789abcdef.jpg"),
        dir.join("01").join("0123456789ABCDEF0123456789ABCDEF.jpg"), // upper case
        dir.join("01").join("0123456789abcdef0123456789abcdef.jpeg"),
        dir.join("01").join("0123456789abcdef0123456789abcdef.tmp"),
    ];
    for p in &foreign {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![7u8; 50_000]).unwrap();
    }
    // a stale temp file of our own is cleaned up
    let own_tmp = dir.join("01").join("0123456789abcdef0123456789abcdef.tmpThreadId9");
    std::fs::write(&own_tmp, b"partial").unwrap();
    let d = DiskCache::new(&dir, 40_000);
    assert_eq!(d.size(), 7, "only the cache's own temp file counts");
    // fill past the budget (forces prunes), then clear
    for i in 0..40u8 {
        d.put(hash_bytes(&[i, 2]), &gradient(160, 100, i));
    }
    assert!(d.size() <= 40_000);
    d.clear();
    assert_eq!(d.size(), 0);
    assert!(!own_tmp.exists());
    for p in &foreign {
        assert!(p.exists(), "foreign file deleted: {}", p.display());
    }
    assert!(disk::is_cache_file("01", "0123456789abcdef0123456789abcdef.jpg"));
    assert!(!disk::is_cache_file("01", "0123456789abcdef0123456789abcdef.jpg.bak"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A shard directory that is a symlink to a folder outside the cache is neither listed, cleared,
/// pruned, read nor written (issue #1504).
#[cfg(unix)]
#[test]
fn disk_cache_ignores_symlinked_shard_directories() {
    let dir = temp_dir("symlink-shard");
    let outside = temp_dir("symlink-outside");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let key = hash_bytes(&[9, 9]);
    let hex = key.to_string();
    let sentinel = outside.join(format!("{hex}.jpg"));
    std::fs::write(&sentinel, vec![7u8; 50_000]).unwrap();
    std::os::unix::fs::symlink(&outside, dir.join(&hex[..2])).unwrap();
    let d = DiskCache::new(&dir, 10);
    assert_eq!(d.size(), 0, "files behind the link are not the cache's");
    d.prune();
    assert!(sentinel.exists(), "prune followed the link");
    assert!(d.get(key).is_none(), "get read through the link");
    d.put(key, &gradient(16, 16, 1));
    assert_eq!(std::fs::metadata(&sentinel).unwrap().len(), 50_000, "put wrote through the link");
    d.clear();
    assert!(sentinel.exists(), "clear followed the link");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&outside);
}

/// Wait up to `secs` for `f` to hold.
fn eventually(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let t0 = std::time::Instant::now();
    while t0.elapsed() < std::time::Duration::from_secs(secs) {
        if f() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}

/// A slider drag submits for a view faster than its renders finish. A newer job for a slot used
/// to start beside the running one on another worker: one view's renders piled up on the GPU and
/// most were thrown away. Now a slot runs one job at a time: the newer one waits (replacing an
/// older one still queued), other slots keep going, and it starts when the slot's job ends.
#[test]
fn a_slot_runs_one_job_at_a_time() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mut p: JobPool<u8, u32> = JobPool::new(4);
    let (release, released) = std::sync::mpsc::channel::<()>();
    let (started, has_started) = std::sync::mpsc::channel();
    let running = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let job = |n: u32, wait: Option<std::sync::mpsc::Receiver<()>>| -> crate::pool::Job<u32> {
        let (running, most, started) = (running.clone(), most.clone(), started.clone());
        Box::new(move || {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(now, Ordering::SeqCst);
            let _ = started.send(n);
            if let Some(w) = wait {
                let _ = w.recv_timeout(std::time::Duration::from_secs(30));
            }
            running.fetch_sub(1, Ordering::SeqCst);
            n
        })
    };
    p.submit(1, 1, 9, job(1, Some(released))).unwrap();
    assert_eq!(has_started.recv().unwrap(), 1);
    // two newer requests for the busy slot: the second replaces the first, neither starts
    p.submit(1, 2, 9, job(2, None)).unwrap();
    p.submit(1, 3, 9, job(3, None)).unwrap();
    // another slot is not held up
    p.submit(2, 9, 1, Box::new(|| 9)).unwrap();
    let mut done = Vec::new();
    assert!(eventually(10, || {
        done.extend(std::iter::from_fn(|| p.try_recv()).map(|d| d.result));
        done.contains(&9)
    }));
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(has_started.try_recv().is_err(), "a second job of slot 1 started beside the first");
    assert!(p.is_queued(1));
    drop(release);
    assert!(eventually(10, || {
        done.extend(std::iter::from_fn(|| p.try_recv()).map(|d| d.result));
        done.contains(&3)
    }));
    assert!(!done.contains(&2), "the replaced job never runs");
    assert_eq!(most.load(Ordering::SeqCst), 1, "slot 1 ran two jobs at once");
}

/// A job that panics still frees its slot: the panic is reported as a worker failure, the worker
/// lives on (Ember catches job panics) and the slot's next job runs.
#[test]
fn a_panicking_job_frees_its_slot() {
    let mut p: JobPool<u8, u32> = JobPool::new(2);
    p.submit(1, 1, 9, Box::new(|| panic!("simulated render panic"))).unwrap();
    let mut failed = false;
    assert!(eventually(10, || {
        failed |= p.try_failure().is_some();
        failed && !p.is_queued(1)
    }));
    p.submit(1, 2, 9, Box::new(|| 2)).unwrap();
    let mut got = None;
    assert!(eventually(10, || {
        got = got.or_else(|| p.try_recv().map(|d| d.result));
        got.is_some()
    }));
    assert_eq!(got, Some(2));
    assert_eq!(p.live_workers(), 2, "the worker survived the panic");
}

/// Deferred disk writes go through one writer with a byte budget: a burst of big renders can't queue more than the
/// budget (each used to start a thread of its own, holding its image; issue #1522), and what was queued reaches disk.
#[test]
fn deferred_writes_are_bounded_and_drain_to_disk() {
    let dir = temp_dir("deferred-budget");
    let cache = Arc::new(PreviewCache::with_disk(1 << 30, &dir, 1 << 31));
    let big = Arc::new(gradient(4096, 2048, 3)); // 32 MiB each
    let keys: Vec<Hash128> = (0..24u32).map(|i| hash_bytes(&i.to_le_bytes())).collect();
    for key in &keys {
        cache.put_deferred(*key, big.clone());
        assert!(cache.queued_bytes() <= MAX_QUEUED_BYTES, "{} bytes queued", cache.queued_bytes());
    }
    let t = std::time::Instant::now();
    while cache.queued_bytes() > 0 && t.elapsed() < std::time::Duration::from_secs(120) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(cache.queued_bytes(), 0, "the queue drains");
    assert!(cache.disk().unwrap().get(keys[0]).is_some(), "the first write reached disk");
    // every image is still in memory, written to disk or not
    assert!(keys.iter().all(|k| cache.get(*k).is_some()));
    let _ = std::fs::remove_dir_all(&dir);
}
