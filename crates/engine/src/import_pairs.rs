//! Conservative pairing of camera RAWs and their rendered JPEG variants.
//!
//! A pair is a presentation hint, never a duplicate: both files remain importable by default.
//! Names alone are deliberately insufficient across folders and exports. Same-folder pairs need
//! matching capture timestamps; a direct export child needs a matching
//! timestamp plus camera metadata. Content-hash duplicate handling stays in `import`.

use std::path::{Path, PathBuf};

use lightcraft_catalog::MediaKind;
use serde::{Deserialize, Serialize};

/// What to do when a camera RAW and JPEG are confidently paired.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RawJpegImportPolicy {
    /// Preserve existing import behaviour: import both files when both are selected.
    #[default]
    KeepBoth,
    /// Select/import the RAW variant and leave its paired JPEG available to keep explicitly.
    RawOnly,
}

impl RawJpegImportPolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "keepboth" | "both" => Some(Self::KeepBoth),
            "rawonly" | "raw" => Some(Self::RawOnly),
            _ => None,
        }
    }
}

/// The kind of relationship shown beside an import candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RawJpegPairKind {
    Raw,
    Jpeg,
}

/// A candidate notice. It does not make either file a duplicate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawJpegPair {
    pub with: String,
    pub kind: RawJpegPairKind,
    /// `sameFolder` or `directExport`.
    pub relation: String,
}

/// Minimal probe facts consumed by the deterministic classifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairInput {
    pub path: String,
    pub kind: MediaKind,
    pub captured: Option<String>,
    pub camera: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairMatch {
    pub raw: usize,
    pub jpeg: usize,
    pub relation: &'static str,
}

const RAW_EXTENSIONS: &[&str] = &["dng", "cr2", "cr3", "nef", "nrw", "arw", "arq", "raf", "orf", "rw2", "rwl", "raw", "pef"];

fn is_raw(input: &PairInput) -> bool {
    input.kind == MediaKind::Raw
        || Path::new(&input.path).extension().is_some_and(|e| RAW_EXTENSIONS.contains(&e.to_string_lossy().to_ascii_lowercase().as_str()))
}

fn is_jpeg(input: &PairInput) -> bool {
    input.kind == MediaKind::Image
        && Path::new(&input.path).extension().is_some_and(|e| matches!(e.to_string_lossy().to_ascii_lowercase().as_str(), "jpg" | "jpeg"))
}

fn stem(path: &str) -> Option<String> {
    Path::new(path).file_stem().map(|s| s.to_string_lossy().to_string())
}

fn same_text(a: &str, b: &str) -> bool {
    if cfg!(windows) { a.eq_ignore_ascii_case(b) } else { a == b }
}

fn same_folder(a: &str, b: &str) -> bool {
    let (Some(ap), Some(bp)) = (Path::new(a).parent(), Path::new(b).parent()) else { return false };
    same_text(&ap.to_string_lossy(), &bp.to_string_lossy())
}

fn direct_export_child(raw: &str, jpeg: &str) -> bool {
    let (Some(parent), Some(jpeg_parent)) = (Path::new(raw).parent(), Path::new(jpeg).parent()) else { return false };
    let parent: Vec<String> = parent.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
    let child: Vec<String> = jpeg_parent.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
    child.len() == parent.len() + 1 && child.iter().zip(&parent).all(|(child, parent)| same_text(child, parent))
}

fn matching_capture(a: &PairInput, b: &PairInput) -> bool {
    match (&a.captured, &b.captured) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

fn matching_camera(a: &PairInput, b: &PairInput) -> bool {
    a.camera.is_empty() || b.camera.is_empty() || same_text(&a.camera, &b.camera)
}

fn strong_export_metadata(raw: &PairInput, jpeg: &PairInput) -> bool {
    raw.captured.as_ref().is_some_and(|capture| jpeg.captured.as_ref() == Some(capture))
        && matching_camera(raw, jpeg)
        && ((!raw.camera.is_empty() && !jpeg.camera.is_empty())
            || raw.width > 0 && raw.height > 0 && jpeg.width > 0 && jpeg.height > 0 && raw.width == jpeg.width && raw.height == jpeg.height)
}

fn stem_key(path: &str) -> Option<String> {
    stem(path).map(|value| if cfg!(windows) { value.to_ascii_lowercase() } else { value })
}

/// Return deterministic matches for confident RAW/JPEG pairs.
pub fn classify(inputs: &[PairInput]) -> Vec<PairMatch> {
    let mut matches = Vec::new();
    let mut used_jpeg = std::collections::HashSet::new();
    let mut jpegs_by_stem = std::collections::HashMap::<String, Vec<usize>>::new();
    for (index, input) in inputs.iter().enumerate().filter(|(_, input)| is_jpeg(input)) {
        if let Some(key) = stem_key(&input.path) {
            jpegs_by_stem.entry(key).or_default().push(index);
        }
    }
    for (raw_index, raw) in inputs.iter().enumerate().filter(|(_, input)| is_raw(input)) {
        let Some(raw_stem) = stem_key(&raw.path) else { continue };
        let Some(jpeg_indices) = jpegs_by_stem.get(&raw_stem) else { continue };
        let mut candidates: Vec<(usize, &'static str)> = jpeg_indices
            .iter()
            .copied()
            .filter_map(|jpeg_index| {
                if used_jpeg.contains(&jpeg_index) {
                    return None;
                }
                let jpeg = &inputs[jpeg_index];
                let relation = if same_folder(&raw.path, &jpeg.path) && matching_capture(raw, jpeg) && matching_camera(raw, jpeg) {
                    Some("sameFolder")
                } else if direct_export_child(&raw.path, &jpeg.path) && strong_export_metadata(raw, jpeg) {
                    Some("directExport")
                } else {
                    None
                }?;
                Some((jpeg_index, relation))
            })
            .collect();
        candidates.sort_by(|a, b| inputs[a.0].path.cmp(&inputs[b.0].path));
        for (jpeg_index, relation) in candidates {
            if used_jpeg.insert(jpeg_index) {
                matches.push(PairMatch { raw: raw_index, jpeg: jpeg_index, relation });
            }
        }
    }
    matches.sort_by_key(|pair| (inputs[pair.raw].path.clone(), inputs[pair.jpeg].path.clone()));
    matches
}

/// Paths excluded by `RawOnly`; caller may still show them with a policy notice.
pub fn excluded_jpegs(inputs: &[PairInput], policy: RawJpegImportPolicy) -> std::collections::HashSet<PathBuf> {
    if policy != RawJpegImportPolicy::RawOnly {
        return std::collections::HashSet::new();
    }
    classify(inputs).into_iter().map(|pair| PathBuf::from(&inputs[pair.jpeg].path)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(path: &str, kind: MediaKind, captured: Option<&str>) -> PairInput {
        PairInput { path: path.into(), kind, captured: captured.map(str::to_string), camera: "Canon EOS R5".into(), width: 6000, height: 4000 }
    }

    #[test]
    fn same_folder_stem_and_matching_capture_are_pair() {
        let files = [
            p("/shots/IMG_001.CR3", MediaKind::Raw, Some("2026-01-01T10:00:00")),
            p("/shots/IMG_001.JPG", MediaKind::Image, Some("2026-01-01T10:00:00")),
        ];
        assert_eq!(classify(&files), [PairMatch { raw: 0, jpeg: 1, relation: "sameFolder" }]);
    }

    #[test]
    fn same_stem_alone_and_different_capture_do_not_pair() {
        let same_stem = [p("/a/IMG_001.CR3", MediaKind::Raw, None), p("/b/IMG_001.JPG", MediaKind::Image, None)];
        assert!(classify(&same_stem).is_empty());
        let different_capture =
            [p("/a/IMG_001.CR3", MediaKind::Raw, Some("2026-01-01T10:00:00")), p("/a/IMG_001.JPG", MediaKind::Image, Some("2026-01-01T10:01:00"))];
        assert!(classify(&different_capture).is_empty());
    }

    #[test]
    fn direct_export_requires_strong_metadata() {
        let weak = [p("/shots/IMG_001.CR3", MediaKind::Raw, Some("2026-01-01T10:00:00")), p("/shots/export/IMG_001.JPG", MediaKind::Image, None)];
        assert!(classify(&weak).is_empty());
        let strong = [
            p("/shots/IMG_001.CR3", MediaKind::Raw, Some("2026-01-01T10:00:00")),
            p("/shots/export/IMG_001.JPG", MediaKind::Image, Some("2026-01-01T10:00:00")),
        ];
        assert_eq!(classify(&strong)[0].relation, "directExport");
    }

    #[test]
    fn raw_only_excludes_only_confident_jpegs() {
        let files = [
            p("/shots/A.CR3", MediaKind::Raw, Some("t")),
            p("/shots/A.JPG", MediaKind::Image, Some("t")),
            p("/shots/B.JPG", MediaKind::Image, Some("t")),
        ];
        let excluded = excluded_jpegs(&files, RawJpegImportPolicy::RawOnly);
        assert_eq!(excluded, [PathBuf::from("/shots/A.JPG")].into_iter().collect());
        assert!(excluded_jpegs(&files, RawJpegImportPolicy::KeepBoth).is_empty());
    }

    #[test]
    fn repeated_raw_stems_use_each_jpeg_once_deterministically() {
        let files = [
            p("/shots/IMG_001.CR3", MediaKind::Raw, Some("t")),
            p("/shots/IMG_001.DNG", MediaKind::Raw, Some("t")),
            p("/shots/IMG_001.JPG", MediaKind::Image, Some("t")),
        ];
        assert_eq!(classify(&files), [PairMatch { raw: 0, jpeg: 2, relation: "sameFolder" }]);
    }

    #[test]
    fn repeated_stem_matches_capture_time_before_path_order() {
        let files = [
            p("/shots/IMG_001.CR3", MediaKind::Raw, Some("first")),
            p("/shots/IMG_001.DNG", MediaKind::Raw, Some("second")),
            p("/shots/IMG_001.JPG", MediaKind::Image, Some("second")),
            p("/shots/IMG_001.jpeg", MediaKind::Image, Some("first")),
        ];
        assert_eq!(classify(&files), [PairMatch { raw: 0, jpeg: 3, relation: "sameFolder" }, PairMatch { raw: 1, jpeg: 2, relation: "sameFolder" },]);
    }

    #[test]
    fn multiple_jpeg_variants_pair_with_same_raw() {
        let files = [
            p("/shots/IMG_001.CR3", MediaKind::Raw, Some("t")),
            p("/shots/IMG_001.JPG", MediaKind::Image, Some("t")),
            p("/shots/IMG_001.jpeg", MediaKind::Image, Some("t")),
        ];
        assert_eq!(classify(&files).len(), 2);
        assert!(classify(&files).iter().all(|pair| pair.raw == 0));
    }

    #[test]
    fn conflicting_camera_metadata_does_not_pair() {
        let mut files = [p("/shots/IMG_001.CR3", MediaKind::Raw, Some("t")), p("/shots/export/IMG_001.JPG", MediaKind::Image, Some("t"))];
        files[1].camera = "Nikon Z8".into();
        assert!(classify(&files).is_empty());
    }
}
