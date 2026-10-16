//! Recognition commands: `faces.index` (embed the faces in the library), `faces.suggest` (who might an unnamed
//! face be?), `faces.person` (one person's faces, and the unnamed faces that look like them), `faces.unnamed` (every
//! unnamed face, look-alikes together) and `faces.setName` / `faces.nameFaces` (name one face, or many at once, which is
//! what teaches the suggestions).
//!
//! Everything here is a suggestion for the user to confirm; nothing is ever named automatically. All of it is
//! catalog-only (no sidecar is written) and `faces.setName` is undoable.

use lightcraft_catalog::Op;
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd};
use crate::{Result, Session};

/// The `name` parameter of the naming commands: a name, or none (`null`, empty) to clear it.
fn name_param(p: &Value, c: &str) -> Result<Option<String>> {
    let name = match p.get("name") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_str().ok_or_else(|| bad(c, "`name` must be text or null"))?.trim().to_string()).filter(|n| !n.is_empty()),
    };
    if let Some(n) = &name
        && (n.chars().count() > 200 || n.chars().any(char::is_control))
    {
        return Err(bad(c, "a name is at most 200 characters, without control characters"));
    }
    Ok(name)
}

/// Give a face region its name. Naming makes the face yours: a later "Detect Faces" run no longer replaces it.
fn apply_name(region: &mut lightcraft_meta::Region, name: &Option<String>) {
    region.name = name.clone();
    if name.is_some() && region.description.as_deref().is_some_and(|d| d.starts_with(super::face_detect::MARK)) {
        region.description = None;
    }
}

/// `faces.nameFaces {faces: [{photo, index}], name}`: name (or clear) many face regions at once, as one undo step: what
/// naming a group of look-alikes does. A face the user marked "not this person" for `name` is left unnamed (counted as
/// `skipped`), so a group can be named without undoing that mark.
fn name_faces(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.nameFaces";
    let name = name_param(p, C)?;
    let list = p.get("faces").and_then(Value::as_array).ok_or_else(|| bad(C, "missing `faces`"))?;
    if list.len() > 5000 {
        return Err(bad(C, "at most 5000 faces at a time"));
    }
    let mut by_photo: std::collections::BTreeMap<u64, Vec<usize>> = std::collections::BTreeMap::new();
    for f in list {
        let (Some(photo), Some(index)) =
            (f.get("photo").and_then(Value::as_u64), f.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()))
        else {
            return Err(bad(C, "each face needs a `photo` and an `index`"));
        };
        by_photo.entry(photo).or_default().push(index);
    }
    let (mut ops, mut named, mut skipped) = (Vec::new(), 0usize, 0usize);
    for (photo, indexes) in by_photo {
        let id = lightcraft_catalog::PhotoId(photo);
        let Some(photo) = s.catalog.photo(id) else { continue };
        let mut meta = photo.meta.clone();
        let before = named;
        for i in indexes {
            let Some(region) = meta.regions.get_mut(i).filter(|r| r.kind == lightcraft_meta::RegionKind::Face) else { continue };
            if name.as_deref().is_some_and(|n| s.catalog.is_face_rejected(id, &region.rect, n)) {
                skipped += 1;
                continue;
            }
            apply_name(region, &name);
            named += 1;
        }
        if named > before {
            ops.push(Op::SetMeta { id, meta: Box::new(meta) });
        }
    }
    if ops.is_empty() {
        return Err(bad(C, if skipped > 0 { "those faces are marked not this person" } else { "none of those faces exist" }));
    }
    let photos = ops.len();
    s.commit(if name.is_some() { "Name Faces" } else { "Clear Face Names" }, Op::Batch { ops })?;
    s.skip_auto_write = true;
    Ok(json!({"named": named, "photos": photos, "name": name, "skipped": skipped}))
}

/// `faces.setName {id?, index, name}`: name (or, with `null` or an empty name, un-name) one face region. Naming
/// makes the face yours: a later "Detect Faces" run no longer replaces it.
fn set_name(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.setName";
    let id = p.get("id").and_then(Value::as_u64).map(lightcraft_catalog::PhotoId).or_else(|| s.active()).ok_or_else(|| bad(C, "no photo"))?;
    let index = p.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).ok_or_else(|| bad(C, "missing or invalid `index`"))?;
    let name = name_param(p, C)?;
    let mut meta = s.catalog.photo(id).ok_or_else(|| bad(C, "no such photo"))?.meta.clone();
    let region = meta.regions.get_mut(index).ok_or_else(|| bad(C, "no such region"))?;
    apply_name(region, &name);
    s.commit(if name.is_some() { "Name Face" } else { "Clear Face Name" }, Op::SetMeta { id, meta: Box::new(meta) })?;
    s.skip_auto_write = true;
    Ok(json!({"id": id.0, "index": index, "name": name}))
}

/// What a face region drawn by hand says in its description, so it can be told from one that detection or XMP made.
pub(crate) const MANUAL: &str = "Drawn in Ember";

/// Whether a face region was drawn by hand (`faces.addRegion`).
pub(crate) fn is_manual(r: &lightcraft_meta::Region) -> bool {
    r.description.as_deref() == Some(MANUAL)
}

/// The photo a command names (`photo`), or the active one.
fn photo_param(s: &Session, p: &Value, c: &str) -> Result<lightcraft_catalog::PhotoId> {
    match p.get("photo") {
        None | Some(Value::Null) => s.active().ok_or_else(|| bad(c, "no photo")),
        Some(v) => v.as_u64().map(lightcraft_catalog::PhotoId).ok_or_else(|| bad(c, "`photo` must be a photo id")),
    }
}

/// `faces.addRegion {photo?, rect: [x, y, w, h], name?}`: draw a face where detection missed one. `rect` is normalized to
/// the photo's upright frame; it must have a positive size and lie inside the photo. The face is marked as drawn by hand
/// (no embedding is made here: the background index embeds it like any other face). Naming makes it yours, as naming
/// does. One undoable step; like naming, the sidecar is written by the next metadata save.
fn add_region(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.addRegion";
    let id = photo_param(s, p, C)?;
    let [x, y, w, h]: [f64; 4] = p
        .get("rect")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().map(|v| v.as_f64().filter(|x| x.is_finite())).collect::<Option<Vec<f64>>>())
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| bad(C, "`rect` is [x, y, w, h]: four finite numbers"))?;
    if !(x >= 0.0 && y >= 0.0 && w > 0.0 && h > 0.0 && x + w <= 1.0 && y + h <= 1.0) {
        return Err(bad(C, "the box must have a positive size and lie inside the photo (each of x, y, w, h in 0 to 1)"));
    }
    let name = name_param(p, C)?;
    let mut meta = s.catalog.photo(id).ok_or_else(|| bad(C, "no such photo"))?.meta.clone();
    let rect = lightcraft_geom::Rect { x0: x, y0: y, x1: x + w, y1: y + h };
    meta.regions.push(lightcraft_meta::Region {
        rect,
        kind: lightcraft_meta::RegionKind::Face,
        name: name.clone(),
        description: Some(MANUAL.into()),
    });
    let index = meta.regions.len() - 1;
    s.commit("Tag Face", Op::SetMeta { id, meta: Box::new(meta) })?;
    s.skip_auto_write = true;
    Ok(json!({"photo": id.0, "index": index, "name": name, "manual": true}))
}

/// `faces.removeRegion {face: {photo, index}}`: delete one face box, drawn by hand or found by detection (or from XMP).
/// One undoable step; like the other face-box edits it does not rewrite the sidecar.
fn remove_region(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.removeRegion";
    let face = p.get("face").ok_or_else(|| bad(C, "missing `face` ({photo, index})"))?;
    let id = face.get("photo").and_then(Value::as_u64).map(lightcraft_catalog::PhotoId).ok_or_else(|| bad(C, "missing `face.photo`"))?;
    let index =
        face.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).ok_or_else(|| bad(C, "missing or invalid `face.index`"))?;
    let mut meta = s.catalog.photo(id).ok_or_else(|| bad(C, "no such photo"))?.meta.clone();
    if !meta.regions.get(index).is_some_and(|r| r.kind == lightcraft_meta::RegionKind::Face) {
        return Err(bad(C, "no such face"));
    }
    let gone = meta.regions.remove(index);
    let manual = is_manual(&gone);
    s.commit("Remove Face", Op::SetMeta { id, meta: Box::new(meta) })?;
    s.skip_auto_write = true;
    Ok(json!({"photo": id.0, "index": index, "name": gone.name, "manual": manual}))
}

/// `faces.mergePeople {from, into}`: every face named `from` (case-insensitively) is named `into`, so two people who are
/// one person become one, as one undo step. Catalog-only, like naming: the sidecars are not touched.
fn merge_people(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.mergePeople";
    let from = p.get("from").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(C, "missing `from`"))?;
    let into = p.get("into").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(C, "missing `into`"))?;
    if into.chars().count() > 200 || into.chars().any(char::is_control) {
        return Err(bad(C, "a name is at most 200 characters, without control characters"));
    }
    let key = from.to_lowercase();
    let (mut ops, mut moved) = (Vec::new(), 0usize);
    for ph in s.catalog.photos().filter(|ph| ph.in_library()) {
        let mut meta = ph.meta.clone();
        let mut here = 0usize;
        for r in meta.regions.iter_mut().filter(|r| r.kind == lightcraft_meta::RegionKind::Face) {
            if r.name.as_deref().map(str::trim).is_some_and(|n| n.to_lowercase() == key) {
                r.name = Some(into.to_string());
                here += 1;
            }
        }
        if here > 0 {
            moved += here;
            ops.push(Op::SetMeta { id: ph.id, meta: Box::new(meta) });
        }
    }
    if ops.is_empty() {
        return Err(bad(C, "nobody is named that"));
    }
    let photos = ops.len();
    s.commit("Merge People", Op::Batch { ops })?;
    s.skip_auto_write = true;
    Ok(json!({"from": from, "into": into, "named": moved, "photos": photos}))
}

/// `faces.rejectName {id, index, name, rejected?: true}`: the face `index` of photo `id` is not `name`. Suggestions and
/// "more like them" leave that pair out from then on, so the next best name can win; the face and its own name are not
/// changed. `rejected: false` takes the mark off again. One undoable step, catalog-only, like naming.
fn reject_name(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.rejectName";
    let id = lightcraft_catalog::PhotoId(p.get("id").and_then(Value::as_u64).ok_or_else(|| bad(C, "missing `id`"))?);
    let index = p.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).ok_or_else(|| bad(C, "missing or invalid `index`"))?;
    let name = p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(C, "missing `name`"))?;
    if name.chars().count() > 200 || name.chars().any(char::is_control) {
        return Err(bad(C, "a name is at most 200 characters, without control characters"));
    }
    let rejected = p.get("rejected").and_then(Value::as_bool).unwrap_or(true);
    let photo = s.catalog.photo(id).ok_or_else(|| bad(C, "no such photo"))?;
    let rect = photo
        .meta
        .regions
        .get(index)
        .filter(|r| r.kind == lightcraft_meta::RegionKind::Face)
        .map(|r| r.rect)
        .ok_or_else(|| bad(C, "no such face"))?;
    s.commit(
        if rejected { "Not This Person" } else { "Clear Not This Person" },
        Op::SetFaceRejection { photo: id, rect, person: name.to_string(), rejected },
    )?;
    s.skip_auto_write = true;
    Ok(json!({"id": id.0, "index": index, "name": name, "rejected": rejected}))
}

/// Most faces `faces.unnamed` lists.
const MAX_UNNAMED: usize = 3000;

/// How the unnamed faces were laid out.
struct Arranged {
    /// Faces that look alike are next to each other (only the ones already embedded can be).
    ordered: bool,
    /// Recognition is on and a model is loaded.
    ready: bool,
    /// A name for some faces, when the faces already named say so with confidence: `(photo, index)` to `(name, score)`.
    suggestions: std::collections::HashMap<(u64, usize), (String, f32)>,
}

/// `faces.unnamed {limit?: 600}`: the faces nobody has named, to be named a group at a time. With recognition running,
/// faces that look alike are next to each other, and a face the named faces recognise carries the name they suggest.
fn unnamed(s: &mut Session, p: &Value) -> Result<Value> {
    let limit = p.get("limit").and_then(Value::as_u64).map_or(600, |n| usize::try_from(n).unwrap_or(600)).clamp(1, MAX_UNNAMED);
    let mut all = unnamed_faces(s);
    let total = all.len();
    let arranged = imp::arrange_unnamed(s, &mut all, limit);
    all.truncate(limit);
    let faces: Vec<Value> = all
        .iter()
        .map(|(photo, index, rect)| {
            let view = s.face_view(lightcraft_catalog::PhotoId(*photo), *rect);
            let suggestion = arranged.suggestions.get(&(*photo, *index)).map(|(name, score)| json!({"name": name, "score": score}));
            json!({"photo": photo, "index": index, "rect": rect_json(rect), "view": rect_json(&view), "suggestion": suggestion})
        })
        .collect();
    Ok(json!({"total": total, "faces": faces, "ordered": arranged.ordered, "ready": arranged.ready}))
}

/// Every unnamed face in the library, in photo order: `(photo, region index, box)`.
fn unnamed_faces(s: &Session) -> Vec<(u64, usize, lightcraft_geom::Rect)> {
    let mut all: Vec<(u64, usize, lightcraft_geom::Rect)> = Vec::new();
    for ph in s.catalog.photos().filter(|ph| ph.in_library()) {
        for (index, r) in ph.meta.regions.iter().enumerate() {
            if r.kind == lightcraft_meta::RegionKind::Face && r.name.is_none() {
                all.push((ph.id.0, index, r.rect));
            }
        }
    }
    all.sort_by_key(|(photo, index, _)| (*photo, *index));
    all
}

/// Most unnamed faces `faces.clusters` groups in one call (the first in photo order). Grouping looks at every pair of
/// the faces in a pass, so a refresh stays near half a gigaflop: faces beyond this wait for the next call.
const MAX_GROUPED: usize = 2000;
/// Faces grouped per pass: the first `GROUP_PASS` together, then the next, and so on (groups never span two passes).
const GROUP_PASS: usize = 1000;
/// Most groups `faces.clusters` lists, largest first.
const MAX_GROUPS: usize = 40;

/// What grouping the unnamed faces found.
struct Grouped {
    /// Recognition is on and a model is loaded (otherwise nothing is grouped).
    ready: bool,
    /// Faces that were grouped (at most `MAX_GROUPED`, in photo order).
    considered: usize,
    /// The groups: `(photo, region index, box)` of each face, largest first.
    groups: Vec<Vec<(u64, usize, lightcraft_geom::Rect)>>,
}

/// `faces.clusters {limit?: 40, minSize?: 2}`: the unnamed faces that look alike, grouped (see `cluster`), largest group
/// first. Each group lists its faces, so `faces.nameFaces` over one group names them all in one undoable step. Read-only.
fn clusters(s: &mut Session, p: &Value) -> Result<Value> {
    let limit = p.get("limit").and_then(Value::as_u64).map_or(MAX_GROUPS, |n| usize::try_from(n).unwrap_or(MAX_GROUPS)).clamp(1, MAX_GROUPS);
    let min = p.get("minSize").and_then(Value::as_u64).map_or(2, |n| usize::try_from(n).unwrap_or(2)).max(1);
    let all = unnamed_faces(s);
    let total = all.len();
    let grouped = imp::group_unnamed(s, &all);
    let groups: Vec<Value> = grouped
        .groups
        .iter()
        .filter(|g| g.len() >= min)
        .take(limit)
        .map(|g| {
            let faces: Vec<Value> = g
                .iter()
                .map(|(photo, index, rect)| {
                    let view = s.face_view(lightcraft_catalog::PhotoId(*photo), *rect);
                    json!({"photo": photo, "index": index, "rect": rect_json(rect), "view": rect_json(&view)})
                })
                .collect();
            json!({"size": g.len(), "faces": faces})
        })
        .collect();
    Ok(json!({"total": total, "considered": grouped.considered, "ready": grouped.ready, "clusters": groups}))
}

/// What looking for faces that resemble a person found.
struct Similar {
    /// `{photo, index, rect, score}` of unnamed faces, the most alike first.
    items: Vec<serde_json::Value>,
    /// Recognition is on and a model is loaded, so an empty list means "none yet" rather than "not looking".
    ready: bool,
    /// Photos the background scan has still to do.
    pending: usize,
}

fn rect_json(r: &lightcraft_geom::Rect) -> Value {
    json!({"x0": r.x0, "y0": r.y0, "x1": r.x1, "y1": r.y1})
}

/// Most confirmed faces `faces.person` lists.
const MAX_CONFIRMED: usize = 5000;

/// `faces.person {name, more?}`: the faces named `name` (case-insensitively) across the library, and, when recognition is
/// running, the unnamed faces that look most like them, to be confirmed one by one.
fn person(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.person";
    let name = p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(C, "missing `name`"))?;
    let key = name.to_lowercase();
    let limit = p.get("more").and_then(Value::as_u64).map_or(120, |n| usize::try_from(n).unwrap_or(120)).min(1000);
    let (mut shown, mut total, mut confirmed) = (name.to_string(), 0usize, Vec::new());
    for ph in s.catalog.photos().filter(|ph| ph.in_library()) {
        for (index, r) in ph.meta.regions.iter().enumerate() {
            let Some(n) = r.name.as_deref().filter(|n| r.kind == lightcraft_meta::RegionKind::Face && n.trim().to_lowercase() == key) else {
                continue;
            };
            if total == 0 {
                shown = n.trim().to_string();
            }
            total += 1;
            if confirmed.len() < MAX_CONFIRMED {
                confirmed.push(json!({"photo": ph.id.0, "index": index, "rect": rect_json(&r.rect), "view": rect_json(&s.face_view(ph.id, r.rect))}));
            }
        }
    }
    let similar = if total > 0 { imp::similar(s, &key, limit) } else { Similar { items: Vec::new(), ready: false, pending: 0 } };
    Ok(
        json!({"name": shown, "total": total, "confirmed": confirmed, "more": similar.items, "ready": similar.ready, "pendingPhotos": similar.pending}),
    )
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use lightcraft_catalog::{Op, Photo, PhotoId};
    use lightcraft_develop::DevelopSettings;
    use lightcraft_faces::matching;
    use lightcraft_faces::runtime::Embedder;
    use lightcraft_geom::Rect;
    use lightcraft_meta::{Region, RegionKind};
    use serde_json::{Value, json};

    use super::super::bad;
    use super::super::face_detect::{LABEL, MARK};
    use super::super::face_models::{finish_downloads, installed_models, read_settings, write_settings};
    use super::Similar;
    use crate::faces_worker::{Done, EDGE, PREFETCH, Pace, Prepared, Worker, max_workers, process, target_workers};
    use crate::{Result, Session};

    /// Defaults for suggestions, until a model's own threshold is known.
    const DEFAULT_THRESHOLD: f32 = 0.45;
    const DEFAULT_MARGIN: f32 = 0.06;
    /// How long a look at the models folder is trusted by the per-frame `faces.pump`.
    const RECHECK: Duration = Duration::from_secs(1);
    /// How often the background pump writes new embeddings to the cache file (appending is cheap; this keeps it to a few
    /// writes a minute, and a session that ends writes the rest).
    const SAVE_EVERY: Duration = Duration::from_secs(3);

    /// The chosen recognition model, loaded, with the index reset to it when it changed. `fresh` looks at the models
    /// folder again; otherwise a look made in the last second is trusted (the UI asks every frame).
    fn current_with(s: &mut Session, cmd: &str, fresh: bool) -> Result<Arc<Embedder>> {
        if !fresh
            && let Some((_, e)) = &s.faces.embedder
            && s.faces.checked.is_some_and(|t| t.elapsed() < RECHECK)
        {
            return Ok(e.clone());
        }
        let dir = s.face_models_dir.clone().ok_or_else(|| bad(cmd, "this build has nowhere to keep face models"))?;
        let id = read_settings(&dir).embedder.ok_or_else(|| bad(cmd, "no recognition model is chosen: add one in Settings > Faces and press Use"))?;
        let installed = installed_models(&dir)
            .into_iter()
            .find(|i| i.manifest.id == id)
            .ok_or_else(|| bad(cmd, "the chosen recognition model is not installed"))?;
        let tag = format!("{id}@{}", installed.manifest.sha256.as_deref().unwrap_or("unknown"));
        s.faces.checked = Some(Instant::now());
        // which photos the detector has searched is kept beside the library (or in memory when it is not on disk)
        let scan_path = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join("face-scanned.bin"));
        if s.faces.scanned.path() != scan_path.as_deref() {
            s.faces.scanned.reset(scan_path);
        }
        if let Some((have, e)) = &s.faces.embedder
            && *have == tag
        {
            return Ok(e.clone());
        }
        let embedder = Arc::new(Embedder::load(&dir.join(&id).join("model.onnx"), &installed.manifest).map_err(|e| bad(cmd, e.to_string()))?);
        let dim = match installed.manifest.output {
            lightcraft_faces::OutputSpec::Embedding { dim } => dim as usize,
            _ => return Err(bad(cmd, "the chosen model is not a recognition model")),
        };
        let cache = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join(format!("face-embeddings-{id}.bin")));
        s.faces.index.reset(&tag, dim, cache);
        s.faces.queue.clear();
        s.faces.queue_stamp = None;
        s.faces.in_flight.clear();
        s.faces.embedder = Some((tag, embedder.clone()));
        Ok(embedder)
    }

    fn current(s: &mut Session, cmd: &str) -> Result<Arc<Embedder>> {
        current_with(s, cmd, true)
    }

    /// Whether the background scan should look for faces in this photo: it is in the library, has no face regions at all
    /// (nothing from XMP, nothing drawn or named, no earlier detection) and has not been searched yet.
    fn needs_search(s: &Session, photo: &Photo) -> bool {
        photo.in_library()
            && !photo.meta.regions.iter().any(|r| r.kind == RegionKind::Face)
            && !s.faces.scanned.contains(photo.id.0)
            && !s.faces.scan_failed.contains(&photo.id.0)
    }

    /// The photo's faces that still need embedding (and, with `search`, the faces to find in it first), with the render job
    /// for its picture. `None` when there is nothing to do.
    fn prepare(s: &mut Session, id: PhotoId, search: bool) -> Option<Prepared> {
        let tag = s.faces.index.tag.clone();
        let photo = s.catalog.photo(id)?.clone();
        let todo: Vec<Rect> =
            photo.meta.regions.iter().filter(|r| r.kind == RegionKind::Face && s.faces.index.needs(id.0, &r.rect)).map(|r| r.rect).collect();
        // finding faces needs the detector the user may not have downloaded: such a photo is left unsearched, not marked as searched
        let detector = crate::cmd::face_detect::detector(s).ok();
        // automatic finding can be switched off (faces.preferences); Detect Faces always works
        let detect = search && detector.is_some() && needs_search(s, &photo) && s.face_models_dir.as_deref().is_none_or(|d| read_settings(d).detect);
        if todo.is_empty() && !detect {
            return None;
        }
        let job = s.preview_job(id, EDGE, EDGE, false, &DevelopSettings::default())?;
        let preview = s.scan_preview_of(&photo);
        Some(Prepared { tag, epoch: s.faces.epoch, id, job, preview, todo, detect, detector })
    }

    /// The faces the detector found in a photo that had none become its face regions: unnamed, marked as detected (so a
    /// manual Detect Faces replaces them), and not an undo step, since the user did not do this.
    fn add_found(s: &mut Session, id: PhotoId, found: &[Rect]) {
        if found.is_empty() {
            return;
        }
        let Some(photo) = s.catalog.photo(id) else { return };
        // the user (or an import) got there first: leave their faces alone
        if photo.meta.regions.iter().any(|r| r.kind == RegionKind::Face) {
            return;
        }
        let mut meta = photo.meta.clone();
        for rect in found {
            meta.regions.push(Region { rect: *rect, kind: RegionKind::Face, name: None, description: Some(format!("{MARK}{LABEL}")) });
        }
        if s.apply_system(Op::SetMeta { id, meta: Box::new(meta) }).is_ok() {
            // regions are not written to XMP, so no sidecar may be rewritten for this
            s.skip_auto_write = true;
        }
    }

    /// Take a finished photo into the index (ignored when it was made for another model or another library). Returns
    /// (embedded, failed).
    fn ingest(s: &mut Session, d: Done) -> (usize, usize) {
        s.faces.in_flight.remove(&d.id);
        if d.epoch != s.faces.epoch || d.tag != s.faces.index.tag {
            return (0, 0);
        }
        if d.detect_wanted {
            match &d.found {
                Some(found) => {
                    s.faces.scanned.insert(d.id.0);
                    add_found(s, d.id, found);
                }
                None => {
                    s.faces.scan_failed.insert(d.id.0);
                }
            }
        }
        let (mut embedded, mut failed) = (0, 0);
        for (rect, v, view) in d.results {
            match v {
                Some(v) if v.len() == s.faces.index.dim => {
                    s.faces.index.insert_with_view(d.id.0, &rect, v, &view);
                    embedded += 1;
                }
                _ => {
                    s.faces.index.skip(d.id.0, &rect);
                    failed += 1;
                }
            }
        }
        (embedded, failed)
    }

    /// Take in whatever the worker has finished.
    fn collect(s: &mut Session) -> (usize, usize) {
        let finished = s.faces.worker.as_ref().map(Worker::finished).unwrap_or_default();
        finished.into_iter().fold((0, 0), |(e, f), d| {
            let (de, df) = ingest(s, d);
            (e + de, f + df)
        })
    }

    /// Embed every face region of `id` the index does not have yet, here and now. Returns (embedded, failed).
    fn embed_photo(s: &mut Session, id: PhotoId, e: &Embedder) -> Result<(usize, usize)> {
        let Some(job) = prepare(s, id, false) else { return Ok((0, 0)) };
        Ok(ingest(s, process(job, e)))
    }

    /// Photos with a face that needs embedding (and, with `search`, photos to look for faces in): those the caller asked
    /// for first, then those with named faces (they are the gallery everything else is compared with), then the rest, and
    /// last the photos that only need searching.
    fn pending(s: &Session, first: &[PhotoId], search: bool) -> Vec<PhotoId> {
        let mut out: Vec<(u8, PhotoId)> = Vec::new();
        for p in s.catalog.photos().filter(|p| p.in_library()) {
            let faces: Vec<_> = p.meta.regions.iter().filter(|r| r.kind == RegionKind::Face).collect();
            let embed = faces.iter().any(|r| s.faces.index.needs(p.id.0, &r.rect));
            if embed || (search && needs_search(s, p)) {
                let rank = if first.contains(&p.id) {
                    0
                } else if faces.iter().any(|r| r.name.is_some()) {
                    1
                } else if embed {
                    2
                } else {
                    3
                };
                out.push((rank, p.id));
            }
        }
        out.sort_by_key(|(rank, id)| (*rank, id.0));
        out.into_iter().map(|(_, id)| id).collect()
    }

    pub fn index(s: &mut Session, p: &Value) -> Result<Value> {
        const C: &str = "faces.index";
        let embedder = current(s, C)?;
        collect(s);
        let budget = Duration::from_millis(p.get("budgetMs").and_then(Value::as_u64).unwrap_or(200).min(600_000));
        let first: Vec<PhotoId> =
            p.get("ids").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).map(PhotoId).collect()).unwrap_or_default();
        let todo = pending(s, &first, false);
        let started = Instant::now();
        let (mut embedded, mut failed, mut done) = (0, 0, 0);
        for id in &todo {
            // at least one photo per call, so a slow model still makes progress; none when only the status is asked for
            if budget.is_zero() || (done > 0 && started.elapsed() >= budget) {
                break;
            }
            let (e, f) = embed_photo(s, *id, &embedder)?;
            (embedded, failed, done) = (embedded + e, failed + f, done + 1);
        }
        if let Err(e) = s.faces.index.save() {
            log::warn!("could not save the face embeddings: {e}");
        }
        Ok(json!({
            "model": s.faces.index.tag,
            "embedded": embedded,
            "failed": failed,
            "photosDone": done,
            "pendingPhotos": todo.len().saturating_sub(done),
            "indexedFaces": s.faces.index.len(),
            "ms": started.elapsed().as_secs_f64() * 1000.0,
        }))
    }

    fn str_scope(p: &Value) -> Option<&str> {
        p.get("scope").and_then(Value::as_str)
    }

    /// Seconds since 1970 of an ISO capture time (`2024-09-20T10:48:45…`, any zone suffix ignored): only for telling
    /// shots taken close together, so the zone does not matter.
    fn capture_secs(iso: &str) -> Option<i64> {
        let n = |a: usize, b: usize| iso.get(a..b)?.parse::<i64>().ok();
        let (y, m, d, hh, mm, ss) = (n(0, 4)?, n(5, 7)?, n(8, 10)?, n(11, 13)?, n(14, 16)?, n(17, 19)?);
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return None;
        }
        // days from civil (proleptic Gregorian)
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        Some((era * 146_097 + doe - 719_468) * 86_400 + hh * 3600 + mm * 60 + ss)
    }

    /// `faces.evaluate`: leave-one-out over the library's named faces, to see how well recognition works on *your*
    /// photos and which threshold to trust. Each named face in turn is treated as unknown and ranked against the
    /// other named faces, ignoring those from shots taken within `burstSecs` of it (near-duplicates would make
    /// everything look easy). Reports counts only.
    pub fn evaluate(s: &mut Session, p: &Value) -> Result<Value> {
        const C: &str = "faces.evaluate";
        let embedder = current(s, C)?;
        collect(s);
        let burst = p.get("burstSecs").and_then(Value::as_i64).unwrap_or(5).clamp(0, 86_400 * 7);
        let margin = p.get("margin").and_then(Value::as_f64).filter(|v| v.is_finite()).map_or(DEFAULT_MARGIN, |v| v as f32);
        let thresholds: Vec<f32> = p
            .get("thresholds")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_f64).filter(|v| v.is_finite()).map(|v| v as f32).take(64).collect())
            .filter(|v: &Vec<f32>| !v.is_empty())
            .unwrap_or_else(|| (4..=16).map(|i| i as f32 * 0.05).collect());
        let budget = Duration::from_millis(p.get("budgetMs").and_then(Value::as_u64).unwrap_or(0).min(3_600_000));
        let started = Instant::now();
        // `scope: "visible"` looks only at the photos the current filter shows (say one year, or one album)
        let scope: Option<std::collections::HashSet<PhotoId>> = (str_scope(p) == Some("visible")).then(|| s.visible_cloned().into_iter().collect());
        let in_scope = |id: PhotoId| scope.as_ref().is_none_or(|v| v.contains(&id));
        let todo: Vec<PhotoId> = pending(s, &[], false).into_iter().filter(|id| in_scope(*id)).collect();
        let mut done = 0;
        for id in &todo {
            if started.elapsed() >= budget {
                break;
            }
            embed_photo(s, *id, &embedder)?;
            done += 1;
        }
        // every named face with its embedding, the photo it is in, and when that was taken
        struct Named {
            name: String,
            emb: Arc<[f32]>,
            photo: PhotoId,
            when: Option<i64>,
        }
        let named: Vec<Named> = s
            .catalog
            .photos()
            .filter(|ph| ph.in_library() && in_scope(ph.id))
            .flat_map(|ph| {
                let when = ph.captured.as_deref().and_then(capture_secs);
                ph.meta
                    .regions
                    .iter()
                    .filter(|r| r.kind == RegionKind::Face)
                    .filter_map(|r| Some(Named { name: r.name.clone()?, emb: s.faces.index.get(ph.id.0, &r.rect)?.clone(), photo: ph.id, when }))
                    .collect::<Vec<_>>()
            })
            .collect();
        let near = |a: &Named, b: &Named| a.photo == b.photo || matches!((a.when, b.when), (Some(x), Some(y)) if (x - y).abs() <= burst);
        let mut sweep: Vec<(f32, usize, usize)> = thresholds.iter().map(|t| (*t, 0, 0)).collect();
        let (mut queries, mut top1, mut unmatchable) = (0usize, 0usize, 0usize);
        for q in &named {
            let others: Vec<&Named> = named.iter().filter(|g| !near(q, g)).collect();
            if !others.iter().any(|g| g.name.eq_ignore_ascii_case(&q.name)) {
                unmatchable += 1;
                continue;
            }
            let refs: Vec<(&str, &[f32])> = others.iter().map(|g| (g.name.as_str(), &g.emb[..])).collect();
            let ranked = matching::rank(&q.emb, &refs);
            queries += 1;
            if ranked.first().is_some_and(|r| r.name.eq_ignore_ascii_case(&q.name)) {
                top1 += 1;
            }
            for (t, suggested, correct) in sweep.iter_mut() {
                if let Some(sg) = matching::suggest(&ranked, *t, margin) {
                    *suggested += 1;
                    if sg.name.eq_ignore_ascii_case(&q.name) {
                        *correct += 1;
                    }
                }
            }
        }
        let pct = |n: usize, d: usize| if d == 0 { Value::Null } else { json!((n as f64 / d as f64 * 1000.0).round() / 1000.0) };
        Ok(json!({
            "model": s.faces.index.tag,
            "namedFaces": named.len(),
            "queries": queries,
            "unmatchable": unmatchable,
            "pendingPhotos": todo.len().saturating_sub(done),
            "top1": pct(top1, queries),
            "margin": margin,
            "burstSecs": burst,
            "sweep": sweep.iter().map(|(t, sg, ok)| json!({"threshold": t, "suggested": sg, "correct": ok, "precision": pct(*ok, *sg), "recall": pct(*ok, queries)})).collect::<Vec<_>>(),
        }))
    }

    /// The scan's row in the activity stack: there while `pending` photos are left, its whole the most that were left at
    /// once since the scan last finished (which it returns; 0 when none are left). It can't be stopped from there: the
    /// scan is switched off in Settings.
    fn track(s: &mut Session, pending: u64) -> u64 {
        if pending == 0 {
            s.faces.task = None;
            s.faces.peak = 0;
            return 0;
        }
        s.faces.peak = s.faces.peak.max(pending);
        let task = s.faces.task.get_or_insert_with(|| s.activity.start("faces", "Finding faces", crate::activity::Cancel::No));
        task.progress(s.faces.peak.saturating_sub(pending), s.faces.peak);
        s.faces.peak
    }

    /// `faces.pump`: cheap to call every frame. Takes in what the background worker has finished and, when it is idle,
    /// hands it the next photo with faces to embed. Does nothing (and says so) until recognition is switched on in
    /// Settings with a model chosen.
    pub fn pump(s: &mut Session, p: &Value) -> Result<Value> {
        const C: &str = "faces.pump";
        // how hard the scan may work now, from what the user is doing (the app says); nothing new is started while paused,
        // and what is already running finishes
        let pace = Pace::parse(p.get("pace").and_then(Value::as_str));
        let target = target_workers(pace);
        // a model that has finished downloading is installed and switched on here, whether or not recognition was on
        finish_downloads(s);
        let enabled = s.face_models_dir.clone().is_some_and(|d| enabled_cached(s, &d));
        if !enabled {
            track(s, 0);
            return Ok(json!({"active": false}));
        }
        if s.faces.retry_at.is_some_and(|t| Instant::now() < t) {
            track(s, 0);
            return Ok(json!({"active": false}));
        }
        let Ok(embedder) = current_with(s, C, false) else {
            // the chosen model is gone or broken: look again in a few seconds, not every frame
            s.faces.retry_at = Some(Instant::now() + Duration::from_secs(3));
            track(s, 0);
            return Ok(json!({"active": false}));
        };
        s.faces.retry_at = None;
        let (embedded, _) = collect(s);
        if (s.faces.index.has_unsaved() || s.faces.scanned.has_unsaved()) && s.faces.saved_at.is_none_or(|t| t.elapsed() >= SAVE_EVERY) {
            if let Err(e) = s.faces.index.save() {
                log::warn!("could not save the face embeddings: {e}");
            }
            if let Err(e) = s.faces.scanned.save() {
                log::warn!("could not save the list of searched photos: {e}");
            }
            s.faces.saved_at = Some(Instant::now());
        }
        if s.faces.in_flight.is_empty() && s.faces.queue.is_empty() && s.faces.queue_stamp != Some(s.catalog.revision) {
            // the queue is taken from the end, so it is made last first
            let mut queue = pending(s, &[], true);
            queue.reverse();
            s.faces.queue = queue;
            s.faces.queue_stamp = Some(s.catalog.revision);
        }
        if target > 0 && s.faces.worker.is_none() && !s.faces.queue.is_empty() {
            s.faces.worker = Worker::start(max_workers());
        }
        // the workers are allowed `target` photos at once; a couple more wait behind them, so a worker that finishes has its
        // next photo at once instead of waiting for the next frame to hand it one
        if let Some(w) = &s.faces.worker {
            w.allow(target, pace);
        }
        while target > 0 && s.faces.in_flight.len() < target + PREFETCH {
            let Some(id) = s.faces.queue.pop() else { break };
            let Some(job) = prepare(s, id, true) else { continue };
            if s.faces.worker.as_ref().is_some_and(|w| w.submit(embedder.clone(), job)) {
                s.faces.in_flight.insert(id);
            } else {
                break;
            }
        }
        let pending = s.faces.queue.len() + s.faces.in_flight.len();
        let peak = track(s, pending as u64);
        Ok(json!({
            "active": true,
            "inFlight": s.faces.in_flight.len(),
            "pendingPhotos": pending,
            "peak": peak,
            "indexedFaces": s.faces.index.len(),
            "searchedPhotos": s.faces.scanned.len(),
            "workers": target,
            "embedded": embedded,
        }))
    }

    /// The unnamed faces that look most like the person whose lower-cased name is `key`, best first, at most `limit`: each
    /// is scored like a suggestion (the mean of its two closest faces of theirs) and kept above four fifths of the
    /// model's suggestion bar, since a person looks at these before anything is named. Only faces already embedded can be
    /// found; `ready` says whether recognition is running at all.
    pub fn similar(s: &mut Session, key: &str, limit: usize) -> Similar {
        let none = Similar { items: Vec::new(), ready: false, pending: 0 };
        let Some(dir) = s.face_models_dir.clone() else { return none };
        if !enabled_cached(s, &dir) {
            return none;
        }
        let Ok(embedder) = current_with(s, "faces.person", false) else { return none };
        let pending = s.faces.queue.len() + s.faces.in_flight.len();
        let (mut gallery, mut candidates): (Vec<Arc<[f32]>>, Vec<(u64, usize, Rect, Arc<[f32]>)>) = (Vec::new(), Vec::new());
        for ph in s.catalog.photos().filter(|ph| ph.in_library()) {
            for (index, r) in ph.meta.regions.iter().enumerate().filter(|(_, r)| r.kind == RegionKind::Face) {
                let Some(e) = s.faces.index.get(ph.id.0, &r.rect) else { continue };
                match r.name.as_deref() {
                    Some(n) if n.trim().to_lowercase() == key => gallery.push(e.clone()),
                    Some(_) => {}
                    None if !s.catalog.is_face_rejected(ph.id, &r.rect, key) => candidates.push((ph.id.0, index, r.rect, e.clone())),
                    None => {}
                }
            }
        }
        let ready = Similar { items: Vec::new(), ready: true, pending };
        if gallery.is_empty() {
            return ready;
        }
        let bar = embedder.manifest().thresholds.match_cosine.unwrap_or(DEFAULT_THRESHOLD) * 0.8;
        let refs: Vec<(&str, &[f32])> = gallery.iter().map(|e| ("person", &e[..])).collect();
        let mut scored: Vec<(f32, u64, usize, Rect)> = candidates
            .into_iter()
            .filter_map(|(photo, index, rect, e)| {
                let score = matching::rank(&e, &refs).first()?.score;
                (score >= bar).then_some((score, photo, index, rect))
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        scored.truncate(limit);
        let items = scored
            .into_iter()
            .map(|(score, photo, index, rect)| {
                let view = s.face_view(PhotoId(photo), rect);
                json!({"photo": photo, "index": index, "rect": super::rect_json(&rect), "view": super::rect_json(&view), "score": score})
            })
            .collect();
        Similar { items, ..ready }
    }

    /// Most faces put in order of likeness (the order is a pass over every pair of them).
    const MAX_ARRANGED: usize = 1500;

    /// Faces in an order that puts look-alikes together: start anywhere, then always go to the closest face not yet placed.
    fn nearest_chain(embeddings: &[&[f32]]) -> Vec<usize> {
        let n = embeddings.len();
        let (mut placed, mut order) = (vec![false; n], Vec::with_capacity(n));
        let mut current = 0;
        if n == 0 {
            return order;
        }
        placed[0] = true;
        order.push(0);
        for _ in 1..n {
            let mut best: Option<(usize, f32)> = None;
            for (j, e) in embeddings.iter().enumerate() {
                if placed[j] {
                    continue;
                }
                let c = matching::cosine(embeddings[current], e).unwrap_or(-1.0);
                if best.is_none_or(|(_, b)| c > b) {
                    best = Some((j, c));
                }
            }
            let Some((j, _)) = best else { break };
            placed[j] = true;
            order.push(j);
            current = j;
        }
        order
    }

    /// Put the unnamed faces `all` in order of likeness (the ones already embedded; the rest follow in photo order) and
    /// find a name for those the named faces recognise, for the first `limit` of them.
    pub fn arrange_unnamed(s: &mut Session, all: &mut Vec<(u64, usize, Rect)>, limit: usize) -> super::Arranged {
        let none = super::Arranged { ordered: false, ready: false, suggestions: Default::default() };
        let Some(dir) = s.face_models_dir.clone() else { return none };
        if !enabled_cached(s, &dir) {
            return none;
        }
        let Ok(embedder) = current_with(s, "faces.unnamed", false) else { return none };
        let (mut embedded, mut plain) = (Vec::new(), Vec::new());
        for f in all.drain(..) {
            match s.faces.index.get(f.0, &f.2) {
                Some(e) => embedded.push((f, e.clone())),
                None => plain.push(f),
            }
        }
        let beyond = embedded.split_off(embedded.len().min(MAX_ARRANGED));
        let order = nearest_chain(&embedded.iter().map(|(_, e)| &e[..]).collect::<Vec<_>>());
        let ordered: Vec<((u64, usize, Rect), Arc<[f32]>)> = order.into_iter().filter_map(|i| embedded.get(i).cloned()).collect();
        // names the faces already named suggest, as `faces.suggest` would
        let gallery: Vec<(String, Arc<[f32]>)> = s
            .catalog
            .photos()
            .filter(|ph| ph.in_library())
            .flat_map(|ph| {
                ph.meta
                    .regions
                    .iter()
                    .filter(|r| r.kind == RegionKind::Face)
                    .filter_map(|r| Some((r.name.clone()?, s.faces.index.get(ph.id.0, &r.rect)?.clone())))
                    .collect::<Vec<_>>()
            })
            .collect();
        let refs: Vec<(&str, &[f32])> = gallery.iter().map(|(n, e)| (n.as_str(), &e[..])).collect();
        let threshold = embedder.manifest().thresholds.match_cosine.unwrap_or(DEFAULT_THRESHOLD);
        let mut suggestions = std::collections::HashMap::new();
        if !refs.is_empty() {
            for ((photo, index, rect), e) in ordered.iter().take(limit) {
                let ranked: Vec<_> =
                    matching::rank(e, &refs).into_iter().filter(|c| !s.catalog.is_face_rejected(PhotoId(*photo), rect, &c.name)).collect();
                if let Some(x) = matching::suggest(&ranked, threshold, DEFAULT_MARGIN) {
                    suggestions.insert((*photo, *index), (x.name, x.score));
                }
            }
        }
        let grouped = !ordered.is_empty();
        all.extend(ordered.into_iter().map(|(f, _)| f));
        all.extend(beyond.into_iter().map(|(f, _)| f));
        all.extend(plain);
        super::Arranged { ordered: grouped, ready: true, suggestions }
    }

    /// Groups the unnamed faces `all` by likeness: the first `MAX_GROUPED` of them that are embedded, in the order given.
    /// The threshold is the match strictness saved in Settings, else the model's own, as `faces.suggest` uses. A face the
    /// user marked "not this person" for a name is kept apart from any face that resembles that name.
    pub fn group_unnamed(s: &mut Session, all: &[(u64, usize, Rect)]) -> super::Grouped {
        let none = super::Grouped { ready: false, considered: 0, groups: Vec::new() };
        let Some(dir) = s.face_models_dir.clone() else { return none };
        if !enabled_cached(s, &dir) {
            return none;
        }
        let Ok(embedder) = current_with(s, "faces.clusters", false) else { return none };
        let threshold = read_settings(&dir).threshold.or(embedder.manifest().thresholds.match_cosine).unwrap_or(DEFAULT_THRESHOLD);
        let faces: Vec<((u64, usize, Rect), Arc<[f32]>)> =
            all.iter().filter_map(|f| Some((*f, s.faces.index.get(f.0, &f.2)?.clone()))).take(super::MAX_GROUPED).collect();
        // the named faces: their name (lower-cased) and embedding
        let mut gallery: Vec<(String, Arc<[f32]>)> = Vec::new();
        for ph in s.catalog.photos().filter(|ph| ph.in_library()) {
            for r in ph.meta.regions.iter().filter(|r| r.kind == RegionKind::Face) {
                if let (Some(n), Some(e)) = (r.name.as_deref(), s.faces.index.get(ph.id.0, &r.rect)) {
                    gallery.push((n.trim().to_lowercase(), e.clone()));
                }
            }
        }
        let names: std::collections::BTreeSet<String> = gallery.iter().map(|(n, _)| n.clone()).collect();
        // the names each unnamed face was marked "not this person" for
        let marked: Vec<Vec<String>> = faces
            .iter()
            .map(|((photo, _, rect), _)| {
                let id = lightcraft_catalog::PhotoId(*photo);
                names.iter().map(String::as_str).filter(|n| s.catalog.is_face_rejected(id, rect, n)).map(str::to_string).collect()
            })
            .collect();
        // the marked names that each face resembles (as a suggestion would score it)
        let marked_names: std::collections::BTreeSet<&str> = marked.iter().flatten().map(String::as_str).collect();
        let mut resembles: Vec<Vec<String>> = vec![Vec::new(); faces.len()];
        for name in marked_names {
            let refs: Vec<(&str, &[f32])> = gallery.iter().filter(|(n, _)| n.as_str() == name).map(|(n, e)| (n.as_str(), &e[..])).collect();
            for (j, (_, e)) in faces.iter().enumerate() {
                if matching::rank(&e[..], &refs).first().is_some_and(|c| c.score >= threshold)
                    && let Some(r) = resembles.get_mut(j)
                {
                    r.push(name.to_string());
                }
            }
        }
        let cannot = |i: usize, j: usize| {
            marked.get(i).is_some_and(|m| m.iter().any(|n| resembles.get(j).is_some_and(|r| r.contains(n))))
        };
        let embs: Vec<&[f32]> = faces.iter().map(|(_, e)| &e[..]).collect();
        let grouped = lightcraft_faces::cluster::cluster(&embs, threshold, super::GROUP_PASS, &cannot);
        let groups: Vec<Vec<(u64, usize, Rect)>> =
            grouped.into_iter().map(|g| g.into_iter().filter_map(|i| faces.get(i).map(|(f, _)| *f)).collect()).collect();
        super::Grouped { ready: true, considered: faces.len(), groups }
    }

    /// Whether face recognition is switched on, looked up at most once a second.
    fn enabled_cached(s: &mut Session, dir: &std::path::Path) -> bool {
        if let Some((t, on)) = s.faces.enabled_seen
            && t.elapsed() < RECHECK
        {
            return on;
        }
        let on = read_settings(dir).enabled;
        s.faces.enabled_seen = Some((Instant::now(), on));
        on
    }

    pub fn suggest(s: &mut Session, p: &Value) -> Result<Value> {
        const C: &str = "faces.suggest";
        let embedder = current(s, C)?;
        collect(s);
        let targets = s.targets(p)?;
        let model_threshold = embedder.manifest().thresholds.match_cosine;
        let num = |k: &str| p.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).map(|v| v as f32);
        // a threshold given with the call wins, then the one saved by faces.preferences, then the model's own
        let saved = s.face_models_dir.as_deref().and_then(|d| read_settings(d).threshold);
        let threshold = num("threshold").or(saved).or(model_threshold).unwrap_or(DEFAULT_THRESHOLD);
        let margin = num("margin").unwrap_or(DEFAULT_MARGIN);
        // the photos asked about are embedded first, then the named faces they are compared with, within the budget
        let budget = Duration::from_millis(p.get("budgetMs").and_then(Value::as_u64).unwrap_or(1500).min(600_000));
        let started = Instant::now();
        let todo = pending(s, &targets, false);
        let mut done = 0;
        for id in &todo {
            // a budget of zero only looks at what is already embedded
            if budget.is_zero() || (done > 0 && started.elapsed() >= budget) {
                break;
            }
            embed_photo(s, *id, &embedder)?;
            done += 1;
        }
        let pending_photos: Vec<u64> = todo.iter().skip(done).map(|id| id.0).collect();
        let gallery: Vec<(String, Arc<[f32]>)> = s
            .catalog
            .photos()
            .filter(|ph| ph.in_library())
            .flat_map(|ph| {
                ph.meta
                    .regions
                    .iter()
                    .filter(|r| r.kind == RegionKind::Face)
                    .filter_map(|r| Some((r.name.clone()?, s.faces.index.get(ph.id.0, &r.rect)?.clone())))
                    .collect::<Vec<_>>()
            })
            .collect();
        let refs: Vec<(&str, &[f32])> = gallery.iter().map(|(n, e)| (n.as_str(), &e[..])).collect();
        let mut photos = Vec::new();
        for id in &targets {
            let Some(photo) = s.catalog.photo(*id) else { continue };
            let mut faces = Vec::new();
            for (index, r) in photo.meta.regions.iter().enumerate().filter(|(_, r)| r.kind == RegionKind::Face && r.name.is_none()) {
                let Some(q) = s.faces.index.get(id.0, &r.rect) else {
                    faces.push(json!({"index": index, "pending": true}));
                    continue;
                };
                // a pair the user said is not this person is left out, so the next best name can win
                let ranked: Vec<_> = matching::rank(q, &refs).into_iter().filter(|c| !s.catalog.is_face_rejected(*id, &r.rect, &c.name)).collect();
                let suggestion = matching::suggest(&ranked, threshold, margin);
                faces.push(json!({
                    "index": index,
                    "suggestion": suggestion.as_ref().map(|x| json!({"name": x.name, "score": x.score, "runnerUp": x.runner_up.as_ref().map(|(n, sc)| json!({"name": n, "score": sc}))})),
                    "candidates": ranked.iter().take(3).map(|c| json!({"name": c.name, "score": c.score, "faces": c.faces})).collect::<Vec<_>>(),
                }));
            }
            photos.push(json!({"id": id.0, "faces": faces}));
        }
        Ok(
            json!({"model": s.faces.index.tag, "threshold": threshold, "margin": margin, "galleryFaces": gallery.len(), "pendingPhotos": pending_photos, "photos": photos}),
        )
    }
    /// An unnamed face the detector found: the only kind of region forgetting removes.
    fn unnamed_detection(r: &Region) -> bool {
        r.kind == RegionKind::Face && r.name.is_none() && super::super::face_detect::is_detected(r)
    }

    /// `faces.forget`: all face data goes. Every embedding and the record of which photos were searched are deleted from
    /// the library folder, the unnamed faces the detector found are removed as one undo step (named faces, drawn ones and
    /// those from XMP stay), and recognition is switched off so the background scan does not rebuild the data by itself.
    pub fn forget(s: &mut Session, _: &Value) -> Result<Value> {
        let embeddings = s.faces.index.len();
        let scanned = s.faces.scanned.len();
        let dir = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
        let scan_path = dir.as_ref().map(|d| d.join("face-scanned.bin"));
        // what is still unwritten goes to the files first; both records are then cleared in memory
        let (tag, dim, index_path) = (s.faces.index.tag.clone(), s.faces.index.dim, s.faces.index.path.clone());
        s.faces.index.reset(&tag, dim, None);
        s.faces.index.path = index_path;
        s.faces.scanned.reset(None);
        let mut files = 0;
        if let Some(dir) = &dir
            && let Ok(entries) = std::fs::read_dir(dir)
        {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let ours = name == "face-scanned.bin" || (name.starts_with("face-embeddings-") && name.ends_with(".bin"));
                if ours && std::fs::remove_file(entry.path()).is_ok() {
                    files += 1;
                }
            }
        }
        s.faces.scanned.reset(scan_path);
        s.faces.scan_failed.clear();
        s.faces.queue.clear();
        s.faces.queue_stamp = None;
        s.faces.in_flight.clear();
        // work begun for the data just forgotten is dropped when it arrives
        s.faces.epoch += 1;
        let mut ops = Vec::new();
        let (mut regions, mut photos) = (0usize, 0usize);
        for photo in s.catalog.photos() {
            let n = photo.meta.regions.iter().filter(|r| unnamed_detection(r)).count();
            if n == 0 {
                continue;
            }
            let mut meta = photo.meta.clone();
            meta.regions.retain(|r| !unnamed_detection(r));
            regions += n;
            photos += 1;
            ops.push(Op::SetMeta { id: photo.id, meta: Box::new(meta) });
        }
        if !ops.is_empty() {
            s.commit("Forget Face Data", Op::Batch { ops })?;
            // regions are not written to XMP, so no sidecar is rewritten
            s.skip_auto_write = true;
        }
        let was_on = match s.face_models_dir.clone() {
            Some(d) => {
                let mut st = read_settings(&d);
                let was = st.enabled;
                st.enabled = false;
                write_settings(&d, &st)?;
                was
            }
            None => false,
        };
        s.faces.enabled_seen = None;
        Ok(json!({
            "embeddings": embeddings,
            "scannedPhotos": scanned,
            "unnamedRegions": regions,
            "photos": photos,
            "filesRemoved": files,
            "recognitionWasOn": was_on,
        }))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::tests_face_recognize::{install_yunet, region, set_regions, setup, temp, two_photos};

        fn unit(parts: &[(usize, f32)]) -> Vec<f32> {
            let mut v = vec![0.0; 64];
            for (i, x) in parts {
                v[*i] = *x;
            }
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            v.iter().map(|x| x / n).collect()
        }

        /// A session with a tiny model loaded (so the index has its model's tag).
        fn loaded(name: &str) -> (Session, std::path::PathBuf) {
            let d = temp(name);
            let (mut s, _) = setup(&d, 64);
            s.execute("faces.index", &json!({"budgetMs": 0})).unwrap();
            (s, d)
        }

        fn finished(s: &Session, id: PhotoId, found: Option<Vec<Rect>>, results: Vec<(Rect, Option<Vec<f32>>, Rect)>) -> Done {
            Done { tag: s.faces.index.tag.clone(), epoch: s.faces.epoch, id, results, detect_wanted: true, found }
        }

        #[test]
        fn faces_found_by_the_scan_become_unnamed_regions_without_an_undo_step() {
            let (mut s, d) = loaded("found");
            let (a, b) = two_photos(&s);
            set_regions(&mut s, b, vec![region(0.1, Some("Ann"))]);
            let (undo, redo) = (s.undo.len(), s.redo.len());
            let found = region(0.3, None).rect;
            let done = finished(&s, a, Some(vec![found]), vec![(found, Some(unit(&[(0, 1.0)])), found)]);
            assert_eq!(ingest(&mut s, done), (1, 0));
            let regions = &s.catalog.photo(a).unwrap().meta.regions;
            assert_eq!(regions.len(), 1);
            assert!(regions[0].kind == RegionKind::Face && regions[0].name.is_none() && regions[0].rect == found);
            assert!(regions[0].description.as_deref().unwrap().starts_with(MARK), "marked as detected");
            // not something the user did, and its embedding is already there
            assert_eq!((s.undo.len(), s.redo.len()), (undo, redo));
            assert!(s.skip_auto_write, "no sidecar is rewritten for it");
            assert!(s.faces.scanned.contains(a.0) && s.faces.index.contains(a.0, &found));
            // a photo that has faces by now (the user, an import) keeps exactly those
            let other = region(0.6, None).rect;
            let done = finished(&s, b, Some(vec![other]), vec![(other, None, other)]);
            ingest(&mut s, done);
            assert_eq!(s.catalog.photo(b).unwrap().meta.regions.len(), 1);
            assert!(s.faces.scanned.contains(b.0));
            let _ = std::fs::remove_dir_all(&d);
        }

        #[test]
        fn a_photo_that_could_not_be_searched_is_left_alone_for_the_session() {
            let (mut s, d) = loaded("scanfail");
            let (a, _) = two_photos(&s);
            assert!(needs_search(&s, s.catalog.photo(a).unwrap()));
            let done = finished(&s, a, None, vec![]);
            ingest(&mut s, done);
            assert!(!s.faces.scanned.contains(a.0), "not remembered as searched: the file may come back");
            assert!(!needs_search(&s, s.catalog.photo(a).unwrap()), "but not retried on every pass");
            assert!(!pending(&s, &[], true).contains(&a));
            let _ = std::fs::remove_dir_all(&d);
        }

        #[test]
        fn only_the_background_scan_looks_for_faces_and_only_in_photos_without_any() {
            let (mut s, d) = loaded("search");
            // finding faces needs the detector, which is not bundled; without it the scan only embeds
            let detector = install_yunet(&mut s);
            let (a, b) = two_photos(&s);
            set_regions(&mut s, b, vec![region(0.1, Some("Ann")), region(0.5, None)]);
            // `faces.index` embeds what is boxed; only the scan (search = true) also searches the rest, last
            assert_eq!(pending(&s, &[], false), vec![b]);
            let scan = pending(&s, &[], true);
            assert_eq!(scan.first(), Some(&b), "named faces first");
            assert!(scan.contains(&a) && scan.len() > 2, "the unsearched photos follow");
            assert!(prepare(&mut s, a, false).is_none(), "nothing to embed in a photo without boxes");
            if detector {
                assert!(prepare(&mut s, a, true).is_some_and(|p| p.detect && p.todo.is_empty()));
            } else {
                assert!(prepare(&mut s, a, true).is_none(), "no detector: nothing to do, and not marked as searched");
            }
            assert!(prepare(&mut s, b, true).is_some_and(|p| !p.detect && p.todo.len() == 2), "a photo with boxes is not searched");
            // once searched, a photo is not searched again
            s.faces.scanned.insert(a.0);
            assert!(!pending(&s, &[], true).contains(&a));
            let _ = std::fs::remove_dir_all(&d);
        }

        #[test]
        fn opening_another_library_forgets_the_old_ones_faces() {
            let (mut s, d) = loaded("newlib");
            let (a, _) = two_photos(&s);
            let rect = region(0.2, None).rect;
            s.faces.index.insert(a.0, &rect, unit(&[(0, 1.0)]));
            s.faces.scanned.insert(a.0);
            let epoch = s.faces.epoch;
            let old = finished(&s, a, Some(vec![rect]), vec![(rect, Some(unit(&[(1, 1.0)])), rect)]);
            s.open_library(d.join("second"), true).unwrap();
            assert_eq!((s.faces.index.len(), s.faces.scanned.len(), s.faces.queue.len(), s.faces.in_flight.len()), (0, 0, 0, 0));
            assert!(s.faces.epoch != epoch && s.faces.embedder.is_none());
            // a worker that was still busy with the old library cannot put its faces into the new one
            assert_eq!(ingest(&mut s, old), (0, 0));
            assert!(s.faces.index.len() == 0 && !s.faces.scanned.contains(a.0));
            let _ = std::fs::remove_dir_all(&d);
        }

        #[test]
        fn the_unnamed_faces_that_look_like_a_person_come_best_first() {
            let (mut s, d) = loaded("similar");
            let (a, b) = two_photos(&s);
            let c = s.catalog.photos().map(|p| p.id).find(|id| *id != a && *id != b).unwrap();
            set_regions(&mut s, a, vec![region(0.1, Some("Ann")), region(0.5, Some("Bob"))]);
            set_regions(&mut s, b, vec![region(0.1, None), region(0.5, None)]);
            set_regions(&mut s, c, vec![region(0.3, None)]);
            let put = |s: &mut Session, id: PhotoId, x: f64, v: Vec<f32>| s.faces.index.insert(id.0, &region(x, None).rect, v);
            put(&mut s, a, 0.1, unit(&[(0, 1.0)])); // Ann
            put(&mut s, a, 0.5, unit(&[(1, 1.0)])); // Bob
            put(&mut s, b, 0.1, unit(&[(0, 0.95), (1, 0.05)])); // looks just like Ann
            put(&mut s, b, 0.5, unit(&[(1, 1.0)])); // looks like Bob, not Ann
            put(&mut s, c, 0.3, unit(&[(0, 0.8), (1, 0.6)])); // somewhat like Ann
            let r = similar(&mut s, "ann", 10);
            assert!(r.ready);
            let found: Vec<(u64, u64)> = r.items.iter().map(|i| (i["photo"].as_u64().unwrap(), i["index"].as_u64().unwrap())).collect();
            assert_eq!(found, vec![(b.0, 0), (c.0, 0)], "best first; the face that looks like Bob is left out");
            assert!(r.items[0]["score"].as_f64().unwrap() > r.items[1]["score"].as_f64().unwrap());
            assert_eq!(similar(&mut s, "ann", 1).items.len(), 1, "capped");
            assert!(similar(&mut s, "nobody", 10).items.is_empty());
            let _ = std::fs::remove_dir_all(&d);
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use serde_json::Value;

    use super::super::bad;
    use crate::{Result, Session};

    pub fn index(_: &mut Session, _: &Value) -> Result<Value> {
        Err(bad("faces.index", "this build cannot run face recognition"))
    }

    pub fn suggest(_: &mut Session, _: &Value) -> Result<Value> {
        Err(bad("faces.suggest", "this build cannot run face recognition"))
    }

    pub fn evaluate(_: &mut Session, _: &Value) -> Result<Value> {
        Err(bad("faces.evaluate", "this build cannot run face recognition"))
    }

    pub fn pump(_: &mut Session, _: &Value) -> Result<Value> {
        Ok(serde_json::json!({"active": false}))
    }

    pub fn forget(_: &mut Session, _: &Value) -> Result<Value> {
        Err(bad("faces.forget", "this build cannot run face recognition"))
    }

    pub fn similar(_: &mut Session, _: &str, _: usize) -> super::Similar {
        super::Similar { items: Vec::new(), ready: false, pending: 0 }
    }

    pub fn group_unnamed(_: &mut Session, _: &[(u64, usize, lightcraft_geom::Rect)]) -> super::Grouped {
        super::Grouped { ready: false, considered: 0, groups: Vec::new() }
    }

    pub fn arrange_unnamed(_: &mut Session, _: &mut Vec<(u64, usize, lightcraft_geom::Rect)>, _: usize) -> super::Arranged {
        super::Arranged { ordered: false, ready: false, suggestions: Default::default() }
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "faces.index",
            "Index Faces",
            [],
            None,
            "{budgetMs?: 200, ids?: [photo ids to do first]} → {embedded, photosDone, pendingPhotos, indexedFaces, ms} — embed the faces in the library with the chosen recognition model, as many photos as fit in the time (at least one); call again until `pendingPhotos` is 0. `budgetMs: 0` only reports",
            super::always,
            imp::index
        ),
        cmd!(
            "faces.suggest",
            "Suggest Names for Faces",
            [],
            None,
            "{ids?, threshold?, margin?, budgetMs?} → {photos: [{id, faces: [{index, suggestion: {name, score, runnerUp} | null, candidates: [{name, score}]}]}]} — who the unnamed faces in these photos might be, from the faces already named in the library. Only suggestions: nothing is named",
            super::always,
            imp::suggest
        ),
        cmd!(
            query "faces.pump",
            "Index Faces in the Background",
            [],
            None,
            "{pace?: pause | light | normal | full} → {active, inFlight, pendingPhotos, indexedFaces, searchedPhotos, workers} — cheap to call every frame: takes in what the background workers have finished and gives them the next photos, as many at once as the pace allows (none when paused, one when light, half the machine's threads when normal, four fifths when full); inactive until face recognition is on in Settings with a model chosen",
            super::always,
            imp::pump
        ),
        cmd!(
            "faces.evaluate",
            "Evaluate Face Recognition",
            [],
            None,
            "{budgetMs?: 0, burstSecs?: 5, margin?, thresholds?: [..], scope?: \"visible\"} → {queries, top1, sweep: [{threshold, suggested, correct, precision, recall}]} — leave-one-out over your named faces: each is ranked against the others (ignoring shots within burstSecs of it) to show how well the chosen model recognises people in *your* photos and which threshold to trust. Faces not yet embedded are done within budgetMs (`faces.index` does them all); counts only",
            super::always,
            imp::evaluate
        ),
        cmd!(
            query "faces.unnamed",
            "Unnamed Faces",
            [],
            None,
            "{limit?: 600} → {total, faces: [{photo, index, rect, view, suggestion: {name, score} | null}], ordered, ready} — the faces nobody has named; with recognition running, look-alikes are next to each other and a face the named ones recognise carries the name they suggest. `view` is the box to show the face by. Name them with `faces.nameFaces`",
            super::always,
            unnamed
        ),
        cmd!(
            "faces.forget",
            "Forget Face Data",
            [],
            None,
            "{} → {embeddings, scannedPhotos, unnamedRegions, photos, filesRemoved, recognitionWasOn} — forget all face data: every embedding and the record of which photos were searched are deleted from the library folder, and the unnamed faces the detector found are removed as one undo step (named faces, drawn ones and those from XMP stay). Face recognition is switched off, so the background scan does not rebuild the data by itself",
            super::always,
            imp::forget
        ),
        cmd!(
            "faces.nameFaces",
            "Name Faces",
            [],
            None,
            "{faces: [{photo, index}], name} → {named, photos, skipped} — name (or, with a null or empty name, clear) many face regions as one undoable step; a face marked \"not this person\" for that name is skipped; the sidecar is not touched",
            super::always,
            name_faces
        ),
        cmd!(
            query "faces.clusters",
            "Unnamed Face Groups",
            [],
            None,
            "{limit?: 40, minSize?: 2} → {total, considered, ready, clusters: [{size, faces: [{photo, index, rect, view}]}]} — the unnamed faces grouped by likeness (average link at the saved match strictness, else the model's own), largest group first; a face marked \"not this person\" for a name stays apart from that name's look-alikes. Read-only: name a group with `faces.nameFaces` over its faces, one undoable step",
            super::always,
            clusters
        ),
        cmd!(
            query "faces.person",
            "Faces of a Person",
            [],
            None,
            "{name, more?: 120} → {name, total, confirmed: [{photo, index, rect}], more: [{photo, index, rect, score}], ready, pendingPhotos} — the faces named `name` across the library, and the unnamed faces that look most like them (best first, already embedded ones only, once recognition is on); confirm one with `faces.setName`",
            super::always,
            person
        ),
        cmd!(
            "faces.mergePeople",
            "Merge People",
            [],
            None,
            "{from, into} → {named, photos} — every face named `from` (case-insensitively) is named `into`, so one person becomes the other; one undoable step; the sidecars are not touched",
            super::always,
            merge_people
        ),
        cmd!(
            "faces.rejectName",
            "Not This Person",
            [],
            None,
            "{id, index, name, rejected?: true} — the face is not `name`: suggestions and \"more like them\" skip that pair from now on (stored in the catalog, persists); rejected: false clears it; one undoable step; the sidecar is not touched",
            super::always,
            reject_name
        ),
        cmd!(
            "faces.addRegion",
            "Tag Face",
            [],
            None,
            "{photo?, rect: [x, y, w, h], name?} → {photo, index, name, manual} — draw a face that detection missed: a box normalized to the photo's upright frame (positive size, inside the photo), marked as drawn by hand; named when `name` is given; one undoable step; the sidecar is written by the next metadata save, like naming",
            super::always,
            add_region
        ),
        cmd!(
            "faces.removeRegion",
            "Remove Face",
            [],
            None,
            "{face: {photo, index}} → {photo, index, name, manual} — remove one face box, drawn by hand or found by detection; one undoable step; the sidecar is not rewritten",
            super::always,
            remove_region
        ),
        cmd!(
            "faces.setName",
            "Name Face",
            [],
            None,
            "{id?, index, name} — name a face region (null or empty clears it); undoable; the sidecar is not touched",
            super::always,
            set_name
        ),
    ]
}
