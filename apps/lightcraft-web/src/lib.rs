//! Ember in the browser: the same egui UI as the desktop app, compiled to WASM.
//!
//! Build with `cargo run -p xtask -- web` (→ `target/web-site/`), then serve that folder over HTTP (see
//! `docs/web.md`). Differences from the desktop host:
//! - the library lives in browser storage (OPFS, or IndexedDB where OPFS can't be written from the
//!   main thread): catalog journal, presets, view state and prefs are mirrored in memory and
//!   flushed in the background ([`files`]); imported originals are stored by content hash
//!   ([`store`]); a new library starts with the procedural demo photos;
//! - renders run in Web Workers, each a second instance of this module ([`wire`], `workers`), and
//!   fall back to the main thread (one job per frame) if workers can't start;
//! - exports render and encode in those workers too (`exports`) and download the file through the
//!   browser from the main thread; without a worker they run on the main thread.
//! - Auto Settings measure in a worker too (`auto`); the values are applied on the main thread as one
//!   undo step.
//!
//! `?bench` in the URL runs a scripted first-paint / slider-latency measurement and logs it to the
//! console (see [`bench`]); `?store=idb` forces the IndexedDB backend, `?store=memory` disables
//! persistence, `?workers=0` renders on the main thread and `?safe` opens the library without
//! decoding any photo (see `safety`).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod backup;
pub mod bench;
pub mod files;
pub mod retry;
pub mod store;
pub mod wire;

#[cfg(target_arch = "wasm32")]
mod auto;
#[cfg(target_arch = "wasm32")]
mod backend;
#[cfg(target_arch = "wasm32")]
mod exports;
#[cfg(target_arch = "wasm32")]
mod safety;
#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
mod workers;
