//! Downloading the AI mask model, only when the user asked for it. The downloader itself (mirrors,
//! resuming, checks, the pure-Rust HTTPS client) is the `lightcraft-fetch` crate; this module
//! says what each model needs and where it may come from.
//!
//! - **MobileSAM** (the default): one 40.6 MB file, ungated, Apache-2.0, pinned by size and SHA-256
//!   ([`MOBILESAM`], [`MOBILESAM_FILES`]); fetched from [`DEFAULT_MIRRORS`] (a pinned Hugging Face
//!   revision) unless the user lists their own mirrors.
//! - **SAM 3** (optional): gated by Meta, so it is never fetched from a built-in location
//!   ([`SAM3_FILES`] pins what the user must supply; see docs/ai-masks.md).
//!
//! No weights are part of Ember; the catalog below points at the upstream files.

pub use lightcraft_fetch::*;

use std::path::Path;

/// Where a model comes from, who may use it and how to check it: what the download dialog shows and
/// what the fetch manifest pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    /// A stable id.
    pub id: &'static str,
    pub name: &'static str,
    /// The weights file as downloaded (its name in the model folder).
    pub file: &'static str,
    /// The exact URL of the weights (a pinned revision, not a moving branch).
    pub url: &'static str,
    pub size_bytes: u64,
    /// SHA-256 of the file (lower-case hex).
    pub sha256: &'static str,
    /// The licence of the model code and weights.
    pub license: &'static str,
    pub license_url: &'static str,
    /// The model's origin: paper and code.
    pub source: &'static str,
    /// What the weights file is, and the one thing a reader should check about its licence.
    pub provenance: &'static str,
}

/// MobileSAM: SAM's prompt encoder and mask decoder behind a 5M-parameter TinyViT image encoder
/// (Zhang et al., "Faster Segment Anything: Towards Lightweight SAM for Mobile Applications", 2023;
/// arXiv 2306.14289). Verified 2026-10-17: the Hugging Face repository `lmz/candle-sam` is public
/// (`gated: false`), revision `8b4cb7c7`, and this file is 40,613,024 bytes with the SHA-256 below
/// (computed over a fresh download of exactly this URL).
pub const MOBILESAM: ModelInfo = ModelInfo {
    id: "mobilesam-tiny-vit",
    name: "MobileSAM (TinyViT)",
    file: crate::MOBILESAM_FILE,
    url: "https://huggingface.co/lmz/candle-sam/resolve/8b4cb7c743f3b3cb8afd212a86ae15b1bbfdac97/mobile_sam-tiny-vitt.safetensors",
    size_bytes: MOBILESAM_SIZE,
    sha256: MOBILESAM_SHA256,
    license: "Apache License 2.0",
    license_url: "https://github.com/ChaoningZhang/MobileSAM/blob/master/LICENSE",
    source: "https://github.com/ChaoningZhang/MobileSAM (paper: https://arxiv.org/abs/2306.14289)",
    provenance: "Float32 safetensors conversion, by the candle maintainers, of the official weights/mobile_sam.pt of \
                 the MobileSAM repository (Apache-2.0, as is the Segment Anything code it builds on). The conversion \
                 repository has no model card of its own, so the licence is the original repository's.",
};

/// Size of the MobileSAM checkpoint (40.6 MB).
pub const MOBILESAM_SIZE: u64 = 40_613_024;
/// SHA-256 of the MobileSAM checkpoint.
pub const MOBILESAM_SHA256: &str = "5ad43a841b57cc798e2837e251c15aa9f602868a1780d195459383d117784cf7";

/// The file MobileSAM needs, pinned by size and SHA-256.
pub const MOBILESAM_FILES: &[FileSpec] =
    &[FileSpec { name: crate::MOBILESAM_FILE, size: Some(MOBILESAM_SIZE), sha256: Some(MOBILESAM_SHA256), max: MOBILESAM_SIZE }];

/// The `facebook/sam3` checkpoint files [`crate::Sam3::load`] needs.
///
/// `model.safetensors` is pinned to the official checkpoint (Hugging Face LFS SHA-256). The two
/// tokenizer files are small text files without a pinned hash yet: they are only parsed by the
/// tokenizer (never executed) and capped in size.
// TODO(maintainer): pin `vocab.json` and `merges.txt` (size + SHA-256) when the files are
// uploaded to Ember's CDN.
pub const SAM3_FILES: &[FileSpec] = &[
    FileSpec { name: "vocab.json", size: None, sha256: None, max: 16 << 20 },
    FileSpec { name: "merges.txt", size: None, sha256: None, max: 16 << 20 },
    FileSpec {
        name: crate::WEIGHTS_FILE,
        size: Some(SAM3_WEIGHTS_SIZE),
        sha256: Some("6d06f0a5f84e435071fe6603e61d0b4cc7b40e0d39d487cfd4d67d8cc11cc14a"),
        max: SAM3_WEIGHTS_SIZE,
    },
];

/// Size of the official `model.safetensors` (3.44 GB).
pub const SAM3_WEIGHTS_SIZE: u64 = 3_439_938_512;

/// Where Ember downloads the default model (MobileSAM) from, in order: each is a base URL, and file
/// `f` is at `<base>/<f>`. The pinned Hugging Face revision of `lmz/candle-sam` (public, not gated);
/// the file is checked against [`MOBILESAM_SIZE`] and [`MOBILESAM_SHA256`] whatever the location.
// TODO(maintainer): add Ember's own CDN location before this one when the model is mirrored on R2
// (the file there must be byte-identical: same size and SHA-256).
pub const DEFAULT_MIRRORS: &[&str] = &["https://huggingface.co/lmz/candle-sam/resolve/8b4cb7c743f3b3cb8afd212a86ae15b1bbfdac97"];

/// Environment variable with extra mirrors (base URLs separated by commas, spaces or newlines).
/// (Named for SAM 3, where it started; it lists locations of the default model's file now.)
pub const MIRRORS_ENV: &str = "LIGHTCRAFT_SAM3_MIRRORS";

/// The mirrors to try, in order: the environment variable's, then the mirrors file's (one base URL
/// per line, `#` comments). A list of the user's own **replaces** [`DEFAULT_MIRRORS`]; without one,
/// the defaults are used.
pub fn mirrors(env: Option<&str>, file: Option<&Path>) -> Vec<String> {
    mirrors_with(env, file, true)
}

/// [`mirrors`] with the built-in location only when `defaults` (tests and offline builds turn it off).
pub fn mirrors_with(env: Option<&str>, file: Option<&Path>, defaults: bool) -> Vec<String> {
    let own = lightcraft_fetch::mirrors(env, file, &[]);
    if !own.is_empty() || !defaults { own } else { lightcraft_fetch::mirrors(None, None, DEFAULT_MIRRORS) }
}

/// Download settings: the defaults, plus [`TOKEN_ENV`] for a gated mirror.
pub fn options() -> Options {
    Options { token_env: Some(TOKEN_ENV), ..Options::default() }
}

/// Environment variable with a bearer token for a mirror's own host (a gated Hugging Face repository).
pub const TOKEN_ENV: &str = "LIGHTCRAFT_SAM3_TOKEN";

/// What to tell the user when there is no mirror for the model.
pub fn no_mirrors_message() -> String {
    format!(
        "no download location is configured for the AI mask model in this build (set {MIRRORS_ENV}, or put the files in the model folder yourself; see docs/ai-masks.md)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrors_come_from_the_environment_the_file_or_the_defaults() {
        let none = Path::new("/nonexistent/mirrors.txt");
        // the user's own list replaces the built-in location
        assert_eq!(mirrors(Some("https://a.example/x/"), Some(none)), vec!["https://a.example/x".to_string()]);
        // without one: the pinned default
        assert_eq!(mirrors(None, Some(none)), DEFAULT_MIRRORS.iter().map(ToString::to_string).collect::<Vec<_>>());
        assert_eq!(mirrors(Some("not a url"), None), mirrors(None, None));
        // (no built-in location when asked not to have one)
        assert!(mirrors_with(None, Some(none), false).is_empty());
        assert_eq!(mirrors_with(Some("https://a.example/x"), None, false), vec!["https://a.example/x".to_string()]);
        assert!(no_mirrors_message().contains(MIRRORS_ENV));
    }

    #[test]
    fn the_mobilesam_catalog_entry_is_pinned() {
        let m = MOBILESAM;
        assert_eq!((m.file, m.size_bytes, m.sha256), (crate::MOBILESAM_FILE, 40_613_024, MOBILESAM_SHA256));
        // a lower-case hex SHA-256
        assert_eq!(m.sha256.len(), 64);
        assert!(m.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert_eq!(m.sha256, "5ad43a841b57cc798e2837e251c15aa9f602868a1780d195459383d117784cf7");
        // the exact upstream file at a pinned commit, not a moving branch
        assert!(m.url.starts_with("https://huggingface.co/lmz/candle-sam/resolve/8b4cb7c743f3b3cb8afd212a86ae15b1bbfdac97/"));
        assert!(m.url.ends_with(m.file) && !m.url.contains("/main/"));
        // the default mirror plus the file name is that URL
        assert_eq!(format!("{}/{}", DEFAULT_MIRRORS[0], m.file), m.url);
        // the permissive licence, with its evidence
        assert_eq!(m.license, "Apache License 2.0");
        assert!(m.license_url.ends_with("/MobileSAM/blob/master/LICENSE"));
        // the fetch manifest says the same
        assert_eq!(MOBILESAM_FILES.len(), 1);
        let f = &MOBILESAM_FILES[0];
        assert_eq!((f.name, f.size, f.sha256, f.max), (m.file, Some(m.size_bytes), Some(m.sha256), m.size_bytes));
        // practical size: a model worth a one-click download
        assert!(m.size_bytes < 50 << 20);
    }

    #[test]
    fn the_sam3_catalog_is_unchanged() {
        assert_eq!(SAM3_WEIGHTS_SIZE, 3_439_938_512);
        assert!(SAM3_FILES.iter().any(|f| f.name == crate::WEIGHTS_FILE && f.size == Some(SAM3_WEIGHTS_SIZE)));
    }
}
