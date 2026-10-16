//! Commands that wait for an original to load (#192).
//!
//! The engine reads originals synchronously, but after a reload they are in browser storage, not
//! in memory: the first read of one fails with [`STILL_LOADING`] and queues its load
//! ([`crate::store::Originals::get`]). Auto Settings and the other commands that measure the
//! photo's pixels would then fail on the first try after a reload and work a moment later.
//!
//! The browser host parks such a command (`Parked`), keeps the page repainting, and runs it
//! again until it works, fails for another reason, or [`MAX_WAIT_MS`] pass; only then does the
//! caller hear the failure. Only commands that change nothing before they read the pixels may be
//! parked ([`waits_for_originals`]), so running one again is the same as running it once.
//! Plain state, no browser calls: the logic is tested natively.

/// Part of the error a read of an original that is not in memory yet fails with.
pub const STILL_LOADING: &str = "is still loading from browser storage";

/// How long a command may wait for originals to load before its failure is reported (ms).
pub const MAX_WAIT_MS: f64 = 60_000.0;

/// How often a parked command runs again (ms).
pub const RETRY_EVERY_MS: u64 = 50;

/// Whether `error` means "the original is not in memory yet".
pub fn is_still_loading(error: &str) -> bool {
    error.contains(STILL_LOADING)
}

/// Commands that read the photo's pixels before changing anything, so a run that failed on a
/// missing original left no trace and may simply be run again: Auto Settings and its siblings
/// (`develop.auto`, `autoTone`, `autoControl`, `autoBatch`, `autoBwMix`, `autoNoise`,
/// `autoWhiteBalance`).
pub fn waits_for_originals(id: &str) -> bool {
    id.starts_with("develop.auto") && id != "develop.autoSync"
}

/// A parked item: what to run again, when it was first tried, and why it failed last.
#[derive(Debug, PartialEq)]
pub struct Waiting<T> {
    pub item: T,
    pub since_ms: f64,
    pub error: String,
}

/// Items waiting for originals, oldest first.
#[derive(Debug)]
pub struct Parked<T>(Vec<Waiting<T>>);

impl<T> Default for Parked<T> {
    fn default() -> Self {
        Parked(Vec::new())
    }
}

impl<T> Parked<T> {
    /// Park `item` (first tried at `since_ms`) after it failed with `error`.
    pub fn park(&mut self, item: T, since_ms: f64, error: String) {
        self.0.push(Waiting { item, since_ms, error });
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Everything parked, split into what is run again (`.0`) and what waited too long (`.1`,
    /// to be failed with its last error). The parked list is empty afterwards: an item that
    /// fails again is parked again with its original `since_ms`.
    pub fn take(&mut self, now_ms: f64) -> (Vec<Waiting<T>>, Vec<Waiting<T>>) {
        let (live, expired): (Vec<_>, Vec<_>) = std::mem::take(&mut self.0).into_iter().partition(|w| now_ms - w.since_ms < MAX_WAIT_MS);
        (live, expired)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_the_loading_error_the_store_makes() {
        let e = format!("web/abc/a.dng: the original {STILL_LOADING}");
        assert!(is_still_loading(&e));
        assert!(!is_still_loading("web/abc/a.dng: not a raw file"));
    }

    #[test]
    fn only_read_first_commands_wait() {
        for id in ["develop.auto", "develop.autoTone", "develop.autoBatch", "develop.autoControl", "develop.autoWhiteBalance", "develop.autoNoise"] {
            assert!(waits_for_originals(id), "{id}");
        }
        for id in ["develop.autoSync", "develop.set", "photo.rate", "app.export", "library.import"] {
            assert!(!waits_for_originals(id), "{id}");
        }
    }

    #[test]
    fn parked_items_retry_until_the_deadline_then_report_their_error() {
        let mut p: Parked<&str> = Parked::default();
        assert!(p.is_empty());
        p.park("a", 1000.0, "first".into());
        p.park("b", 1000.0 + MAX_WAIT_MS - 10.0, "second".into());
        assert_eq!(p.len(), 2);
        let (live, expired) = p.take(1000.0 + MAX_WAIT_MS - 5.0);
        assert_eq!(live.len(), 2, "both still within the wait");
        assert!(expired.is_empty() && p.is_empty(), "take empties the list");
        // "a" fails again, keeping its first-try time; later it has waited too long
        for w in live {
            p.park(w.item, w.since_ms, w.error);
        }
        let (live, expired) = p.take(1000.0 + MAX_WAIT_MS + 1.0);
        assert_eq!(live.iter().map(|w| w.item).collect::<Vec<_>>(), ["b"]);
        assert_eq!(expired.iter().map(|w| (w.item, w.error.as_str())).collect::<Vec<_>>(), [("a", "first")]);
    }
}
