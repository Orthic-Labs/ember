use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

use serde::Serialize;
use serde_json::{Map, Value, json};

use lightcraft_catalog::{Album, ColorLabel, Flag, Photo, PhotoId};
use lightcraft_develop::controls;
use lightcraft_engine::Session;

use crate::tasks::Tasks;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandInfo {
    pub id: String,
    pub label: String,
    pub menu: Vec<String>,
    pub shortcut: Option<String>,
    pub params: String,
    pub enabled: bool,
    #[serde(rename = "disabled_reason", skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlSpec {
    pub id: String,
    pub label: String,
    pub section: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub step: f64,
    pub decimals: u8,
    pub track: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhotoSummary {
    pub id: u64,
    pub file_name: String,
    pub format: String,
    pub kind: String,
    pub w: u32,
    pub h: u32,
    pub captured: Option<String>,
    pub imported: String,
    pub rating: u8,
    pub flag: Flag,
    pub label: Option<ColorLabel>,
    pub edited: bool,
    pub title: String,
    pub keywords: Vec<String>,
    pub camera: String,
    pub lens: String,
    pub shutter: String,
    pub aperture: Option<f32>,
    pub iso: Option<u32>,
    pub focal_mm: Option<f32>,
    pub deleted: bool,
    pub copy_of: Option<u64>,
    pub copy_name: Option<String>,
    pub preview_only: bool,
}

impl PhotoSummary {
    fn from_photo(photo: &Photo) -> Result<Self, String> {
        crate::validate_id(photo.id.0)?;
        if let Some(copy_of) = photo.copy_of {
            crate::validate_id(copy_of.0)?;
        }
        Ok(Self {
            id: photo.id.0,
            file_name: photo.file_name.clone(),
            format: photo.format.clone(),
            kind: serde_json::to_value(photo.kind).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "image".into()),
            w: photo.width,
            h: photo.height,
            captured: photo.captured.clone(),
            imported: photo.imported.clone(),
            rating: photo.rating,
            flag: photo.flag,
            label: photo.label,
            edited: photo.is_edited(),
            title: photo.meta.title.clone(),
            keywords: photo.meta.keywords.clone(),
            camera: photo.meta.camera.clone(),
            lens: photo.meta.lens.clone(),
            shutter: photo.meta.shutter.clone(),
            aperture: photo.meta.aperture,
            iso: photo.meta.iso,
            focal_mm: photo.meta.focal_mm,
            deleted: photo.deleted,
            copy_of: photo.copy_of.map(|id| id.0),
            copy_name: photo.copy_name.clone(),
            preview_only: photo.preview_only.is_some(),
        })
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumSummary {
    pub id: u64,
    pub name: String,
    pub parent: Option<u64>,
    pub count: usize,
    pub smart: bool,
    pub folder: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobStatus {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub completed: usize,
    pub total: usize,
    pub cancellable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalTask {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStatus {
    pub unsaved: bool,
    pub importing: bool,
    pub exporting: bool,
    pub preview_build: bool,
    pub jobs: Vec<JobStatus>,
    pub completed_jobs: Vec<TerminalTask>,
    pub notices: Vec<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewSlice {
    pub generation: u64,
    pub total: usize,
    pub offset: usize,
    pub photos: Vec<PhotoSummary>,
}

#[derive(Default)]
pub(crate) struct CatalogSnapshotCache {
    revision: Option<u64>,
    albums: Vec<AlbumSummary>,
    counts: BTreeMap<String, usize>,
}

pub fn view_slice(session: &mut Session, generation: Option<u64>, offset: usize, limit: usize) -> Result<Value, String> {
    let limit = limit.min(512);
    let (current_generation, visible) = session.visible_shared();
    let start = offset.min(visible.len());
    let end = start.saturating_add(limit).min(visible.len());
    let mut photos = Vec::with_capacity(end.saturating_sub(start));
    for id in &visible[start..end] {
        crate::validate_id(id.0)?;
        if let Some(photo) = session.catalog.photo(*id) {
            photos.push(PhotoSummary::from_photo(photo)?);
        }
    }
    let result = ViewSlice { generation: current_generation, total: visible.len(), offset: start, photos };
    let mut value = serde_json::to_value(result).map_err(|error| error.to_string())?;
    if generation.is_some_and(|requested| requested != current_generation)
        && let Some(object) = value.as_object_mut()
    {
        object.insert("generationChanged".into(), Value::Bool(true));
    }
    Ok(value)
}

pub fn snapshot(
    session: &mut Session,
    tasks: &Tasks,
    preferences: &Map<String, Value>,
    notices: &[String],
    error: Option<String>,
    cache: &mut CatalogSnapshotCache,
) -> Result<Value, String> {
    let (generation, visible) = session.visible_shared();
    let active = session.active().map(|id| id.0);
    if let Some(id) = active {
        crate::validate_id(id)?;
    }
    let develop = active.and_then(|id| session.develop_of(PhotoId(id))).map(|d| d.to_json()).unwrap_or(Value::Object(Map::new()));
    let settings = serde_json::from_value::<lightcraft_develop::DevelopSettings>(develop.clone()).unwrap_or_default();
    let mut control_values = BTreeMap::new();
    for control in controls::CONTROLS {
        if let Some(value) = controls::get(&settings, control.id) {
            control_values.insert(control.id.to_string(), value);
        }
    }
    for (id, _control) in controls::indexed_instances(&settings) {
        if let Some(value) = controls::get(&settings, &id) {
            control_values.insert(id, value);
        }
    }
    let commands = session
        .commands()
        .into_iter()
        .map(|command| CommandInfo {
            id: command.id.to_string(),
            label: command.label.to_string(),
            menu: command.menu.into_iter().map(str::to_string).collect(),
            shortcut: command.shortcut.map(str::to_string),
            params: command.params.to_string(),
            enabled: command.enabled,
            disabled_reason: command.disabled_reason,
        })
        .collect::<Vec<_>>();
    let mut controls = controls::CONTROLS
        .iter()
        .map(|control| ControlSpec {
            id: control.id.into(),
            label: control.label.into(),
            section: control.section.label().into(),
            min: control.min,
            max: control.max,
            default: control.default,
            step: control.step,
            decimals: control.decimals,
            track: serde_json::to_value(control.track).unwrap_or(Value::Null),
        })
        .collect::<Vec<_>>();
    controls.extend(controls::indexed_instances(&settings).into_iter().map(|(id, control)| ControlSpec {
        id,
        label: control.label.into(),
        section: control.section.label().into(),
        min: control.min,
        max: control.max,
        default: control.default,
        step: control.step,
        decimals: control.decimals,
        track: serde_json::to_value(control.track).unwrap_or(Value::Null),
    }));
    let (albums, cached_counts) = cache.derived(session)?;
    let mut counts = cached_counts.clone();
    counts.insert("visible".into(), visible.len());
    let history = active
        .and_then(|id| session.catalog.photo(PhotoId(id)))
        .map(|photo| photo.history.iter().map(|step| json!({"label": step.label})).collect::<Vec<_>>())
        .unwrap_or_default();
    let status_jobs = tasks.statuses();
    let completed_jobs = tasks.completed_jobs();
    let importing = tasks.running_kind("import");
    let exporting = tasks.running_kind("export");
    let preview_build = session.preview_build.as_ref().is_some_and(|build| !build.finished.load(Ordering::Relaxed)) || tasks.running_kind("preview");
    let selection = session.selection.ids.iter().map(|id| crate::validate_id(id.0).map(|_| id.0)).collect::<Result<Vec<_>, _>>()?;
    let status = HostStatus {
        unsaved: session.unsaved().is_some(),
        importing,
        exporting,
        preview_build,
        jobs: status_jobs,
        completed_jobs,
        notices: notices.to_vec(),
        error,
    };
    let value = json!({
        "version": 1,
        "revision": session.catalog.revision,
        "viewGeneration": generation,
        "total": visible.len(),
        "active": active,
        "activeMask": session.active_mask,
        "activeIndex": active.and_then(|id| visible.iter().position(|photo| photo.0 == id)),
        "selection": selection,
        "source": serde_json::to_value(session.source).unwrap_or(Value::Null),
        "filter": serde_json::to_value(&session.filter).unwrap_or(Value::Null),
        "browse": session.browse,
        "libraryFolder": session.library_folder,
        "sort": format!("{:?}", session.sort),
        "commands": commands,
        "controls": controls,
        "controlValues": control_values,
        "develop": develop,
        "albums": albums,
        "counts": counts,
        "undo": session.undo.len(),
        "redo": session.redo.len(),
        "history": history,
        "status": status,
        "libraryPath": session.library.as_ref().map(|library| library.dir.to_string_lossy().to_string()),
        "preferences": Value::Object(preferences.clone()),
    });
    Ok(value)
}

impl CatalogSnapshotCache {
    fn derived(&mut self, session: &Session) -> Result<(&[AlbumSummary], &BTreeMap<String, usize>), String> {
        let revision = session.catalog.revision;
        if self.revision == Some(revision) {
            return Ok((&self.albums, &self.counts));
        }
        let albums = session
            .catalog
            .albums()
            .map(|album| {
                crate::validate_id(album.id.0)?;
                if let Some(parent) = album.parent {
                    crate::validate_id(parent.0)?;
                }
                Ok(album_summary(session, album))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let mut counts = BTreeMap::new();
        let (edited, picks, rejects, deleted) = session.catalog.photos().fold((0, 0, 0, 0), |(edited, picks, rejects, deleted), photo| {
            (
                edited + usize::from(photo.is_edited()),
                picks + usize::from(photo.flag == Flag::Pick),
                rejects + usize::from(photo.flag == Flag::Reject),
                deleted + usize::from(photo.deleted),
            )
        });
        counts.insert("catalog".into(), session.catalog.len());
        counts.insert("edited".into(), edited);
        counts.insert("picks".into(), picks);
        counts.insert("rejects".into(), rejects);
        counts.insert("deleted".into(), deleted);
        counts.insert("albums".into(), albums.len());
        self.revision = Some(revision);
        self.albums = albums;
        self.counts = counts;
        Ok((&self.albums, &self.counts))
    }
}

fn album_summary(session: &Session, album: &Album) -> AlbumSummary {
    AlbumSummary {
        id: album.id.0,
        name: album.name.clone(),
        parent: album.parent.map(|id| id.0),
        count: session.catalog.album_count(album.id),
        smart: album.is_smart(),
        folder: album.folder,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_mask_snapshot_follows_selection_instead_of_last_mask() {
        let mut session = Session::with_demo();
        session.execute("mask.add", &json!({"kind": "radial"})).unwrap();
        let first = session.active_mask.unwrap();
        session.execute("mask.add", &json!({"kind": "brush"})).unwrap();
        assert_ne!(session.active_mask, Some(first));
        session.execute("mask.select", &json!({"id": first})).unwrap();
        let value = snapshot(&mut session, &Tasks::new(), &Map::new(), &[], None, &mut CatalogSnapshotCache::default()).unwrap();
        assert_eq!(value["activeMask"], json!(first));
        session.active_mask = None;
        let value = snapshot(&mut session, &Tasks::new(), &Map::new(), &[], None, &mut CatalogSnapshotCache::default()).unwrap();
        assert!(value["activeMask"].is_null());
    }
}
