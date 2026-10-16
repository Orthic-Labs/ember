//! Lens profile commands: the manual make / model picker (issue #218).
//!
//! Matching a raw to a lens database profile is automatic ([`crate::lens_profiles`]). `develop.lensProfile` overrides
//! it for the target photos: the picked lens is stored in the develop settings
//! ([`DevelopSettings::lens_override`]) and its correction, resolved against the lens database at the photo's focal
//! length, replaces the photo's lens data. Without a pick the automatic match applies again.

use lightcraft_catalog::{Op, PhotoId};
use lightcraft_develop::LensOverride;
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, has_selection, str_param};
use crate::{Result, Session};

const LABEL: &str = "Lens Profile";

/// The lens data of photo `id`'s file for `pick` (`None` = the automatic result), read from the file. `Err` says
/// why the photo can't take it; `Ok(None)` is a file without any usable lens data.
#[cfg(not(target_arch = "wasm32"))]
fn lens_of(s: &Session, id: PhotoId, pick: Option<&LensOverride>) -> std::result::Result<Option<lightcraft_develop::EmbeddedLens>, String> {
    let Some(lightcraft_catalog::Source::File { path }) = s.catalog.photo(id).map(|p| &p.source) else {
        return Err("not a file on disk".into());
    };
    let bytes = std::fs::read(path).map_err(|e| format!("can't read {path}: {e}"))?;
    Ok(crate::lens_profiles::for_bytes(&bytes, pick))
}

#[cfg(target_arch = "wasm32")]
fn lens_of(_: &Session, _: PhotoId, _: Option<&LensOverride>) -> std::result::Result<Option<lightcraft_develop::EmbeddedLens>, String> {
    Err("lens profiles need the file on disk".into())
}

/// `develop.lensProfile {make?, model?, ids?}`: pick the lens database profile `model` (of `make`) for the photos, or
/// with no `model` go back to the automatic match.
fn set_profile(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "develop.lensProfile";
    let ids = s.targets(p)?;
    if ids.is_empty() {
        return Err(bad(C, "no photo selected"));
    }
    let pick = match str_param(p, "model").map(str::trim).filter(|m| !m.is_empty()) {
        None => None,
        Some(model) => {
            let make = str_param(p, "make").unwrap_or_default();
            let db = crate::lens_profiles::database().ok_or_else(|| bad(C, format!("no lens database: set {}", crate::lens_profiles::ENV)))?;
            let lens = db.find_exact(make, model).ok_or_else(|| bad(C, format!("no lens profile named {model:?}")))?;
            Some(LensOverride { make: lens.maker.clone(), model: lens.models.first().cloned().unwrap_or_else(|| model.to_string()) })
        }
    };
    let mut ops = Vec::new();
    let mut changed = Vec::new();
    let mut skipped = Vec::new();
    for id in ids {
        let Some(ph) = s.catalog.photo(id) else {
            skipped.push(json!({"id": id.0, "reason": "no such photo"}));
            continue;
        };
        let lens = match lens_of(s, id, pick.as_ref()) {
            Ok(Some(lens)) => Some(lens),
            // the automatic result may well be "no profile"; a picked profile that gives nothing is a refusal
            Ok(None) if pick.is_none() => None,
            Ok(None) => {
                skipped.push(json!({"id": id.0, "reason": "the profile has no correction for this photo (focal length, or not a raw)"}));
                continue;
            }
            Err(e) => {
                skipped.push(json!({"id": id.0, "reason": e}));
                continue;
            }
        };
        let mut d = (*ph.develop).clone();
        d.lens_override = pick.clone();
        if pick.is_some() {
            d.optics.lens_profile = true;
        }
        ops.push(Op::SetEmbeddedLens { id, lens: lens.map(Box::new) });
        ops.extend(s.develop_op(id, d, LABEL));
        changed.push(id.0);
    }
    if changed.is_empty() {
        let why = skipped.first().and_then(|v| v["reason"].as_str()).unwrap_or("nothing to change");
        return Err(bad(C, why));
    }
    s.commit(LABEL, Op::Batch { ops })?;
    Ok(json!({"changed": changed, "skipped": skipped, "lens": pick.map(|l| json!({"make": l.make, "model": l.model}))}))
}

/// `develop.lensProfiles {}`: the lens database's lenses and the active photo's pick →
/// `{available, lenses: [{make, model}], picked: {make, model}|null}`.
fn list_profiles(s: &mut Session, _p: &Value) -> Result<Value> {
    let lenses: Vec<Value> = crate::lens_profiles::choices().iter().map(|(make, model)| json!({"make": make, "model": model})).collect();
    let picked = s.active().and_then(|id| s.develop_of(id)).and_then(|d| d.lens_override.clone()).map(|l| json!({"make": l.make, "model": l.model}));
    Ok(json!({"available": !lenses.is_empty(), "lenses": lenses, "picked": picked}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "develop.lensProfile",
            "Pick Lens Profile",
            [],
            None,
            "{make?, model?, ids?}: pick the lens database profile `model` (of `make`) for the photos (the target `ids`, else the selection) in place of the automatic match, and turn lens corrections on; no `model` returns to the automatic match. One undo step. Needs the lens database (docs/lens-profiles.md) → {changed: [ids], skipped: [{id, reason}], lens}",
            has_selection,
            set_profile
        ),
        cmd!(
            query "develop.lensProfiles",
            "List Lens Profiles",
            [],
            None,
            "{}: the lens database's lenses, sorted, and the active photo's picked profile → {available, lenses: [{make, model}], picked}",
            always,
            list_profiles
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_database_or_a_photo_the_commands_refuse() {
        let mut s = Session::with_demo();
        // no photo selected
        s.selection = crate::Selection::default();
        assert!(s.execute("develop.lensProfile", &json!({"model": "X"})).is_err());
        let v = s.execute("develop.lensProfiles", &json!({})).unwrap();
        assert!(v["lenses"].is_array() && v["picked"].is_null());
        // a demo photo has no file to read a lens from, and the test process has no lens database: nothing changes
        let id = s.visible_cloned().into_iter().next().unwrap();
        let before = s.develop_of(id).unwrap();
        assert!(s.execute("develop.lensProfile", &json!({"ids": [id.0], "model": "X"})).is_err());
        assert!(s.execute("develop.lensProfile", &json!({"ids": [id.0]})).is_err());
        assert_eq!(s.develop_of(id).unwrap(), before);
    }
}
