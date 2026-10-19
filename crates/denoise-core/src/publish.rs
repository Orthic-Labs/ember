//! Publishing rebuildable cache files whole: through `rightkit-fs` on native targets, which write
//! a temp file next to the destination and rename it over (a reader never sees half a file).
//! The browser build has no file system, so there it reports `Unsupported`.

use std::io::Write;
use std::path::Path;

/// Replace (or create) `path` with what `fill` writes. No fsync: every caller's file can be made
/// again. The new file counts as written when only a step after the replace failed.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn replace_with(path: &Path, fill: impl FnOnce(&mut dyn Write) -> std::io::Result<()>) -> std::io::Result<()> {
    let options = rightkit_fs::AtomicWriteOptions { durable: false, ..rightkit_fs::AtomicWriteOptions::default() };
    match rightkit_fs::atomic_write_with_options(path, options, |file| {
        let mut out = std::io::BufWriter::new(file);
        fill(&mut out)?;
        out.flush()
    }) {
        Ok(()) => Ok(()),
        Err(e) if e.target_replaced() => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn replace_with(_path: &Path, _fill: impl FnOnce(&mut dyn Write) -> std::io::Result<()>) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "this build has no file system"))
}
