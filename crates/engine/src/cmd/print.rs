//! `print.*`: the Classic Print job, its layout, pages, user templates and export (see [`crate::print`]).
//! The print view is UI (`print.view`).

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};
use crate::activity::{Cancel, TaskGuard};
use crate::export::{encode_image, write_file};
use crate::print::{self, PrintTemplate};
use crate::slideshow;
use crate::{Result, Session};

/// Longest print template name, in characters.
const MAX_NAME: usize = 60;

/// The user's template names, in the order they were saved.
fn user_names(s: &Session) -> Vec<&str> {
    s.print_templates.iter().map(|t| t.name.as_str()).collect()
}

fn get(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(json!({"model": s.print, "templates": print::builtin_templates(), "userTemplates": user_names(s)}))
}

fn index_param(p: &Value, id: &str, key: &str) -> Result<usize> {
    let i = p.get(key).and_then(Value::as_u64).ok_or_else(|| bad(id, format!("missing `{key}`")))?;
    usize::try_from(i).map_err(|_| bad(id, format!("`{key}` is out of range")))
}

/// The `ppi` the command names, else `default` (the job's resolution).
fn ppi_param(p: &Value, id: &str, default: f64) -> Result<f64> {
    match p.get("ppi") {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_f64().ok_or_else(|| bad(id, "ppi must be a number")),
    }
}

fn path_param<'a>(p: &'a Value, key: &str) -> Option<&'a str> {
    p.get(key).and_then(Value::as_str).filter(|x| !x.trim().is_empty())
}

/// The trimmed text under `key`; a name is required.
fn name_key<'a>(p: &'a Value, key: &str, id: &str) -> Result<&'a str> {
    p.get(key).and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(id, format!("missing `{key}`")))
}

/// Refuses a user template name that is too long or that a built-in template already has.
fn check_user_name(id: &str, name: &str) -> Result<()> {
    if name.chars().count() > MAX_NAME {
        return Err(bad(id, format!("a print template name is at most {MAX_NAME} characters")));
    }
    if print::builtin_templates().iter().any(|b| b.eq_ignore_ascii_case(name)) {
        return Err(bad(id, format!("`{name}` is a built-in template; choose another name")));
    }
    Ok(())
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.set";
    s.print = print::merged(&s.print, p).map_err(|e| bad(ID, e))?;
    Ok(json!({"model": s.print}))
}

/// Makes a built-in or user template the layout, image and package; the job and page options stay.
fn apply_template(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.applyTemplate";
    let name = name_key(p, "name", ID)?;
    let t = match print::template(name) {
        Ok(m) => PrintTemplate { name: name.to_string(), layout: m.layout, image: m.image, package: m.package },
        Err(_) => s
            .print_templates
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
            .cloned()
            .ok_or_else(|| bad(ID, format!("no print template `{name}`")))?,
    };
    let before = s.print.clone();
    s.print.layout = t.layout;
    s.print.image = t.image;
    s.print.package = t.package;
    if let Err(e) = s.print.validate() {
        s.print = before;
        return Err(bad(ID, e));
    }
    Ok(json!({"model": s.print}))
}

/// Saves the current layout, image and package as a user template; a user template of the same name
/// (ignoring case) is replaced.
fn save_template(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.saveTemplate";
    let name = name_key(p, "name", ID)?;
    check_user_name(ID, name)?;
    let t = PrintTemplate {
        name: name.to_string(),
        layout: s.print.layout.clone(),
        image: s.print.image.clone(),
        package: s.print.package.clone(),
    };
    match s.print_templates.iter_mut().find(|x| x.name.eq_ignore_ascii_case(name)) {
        Some(x) => *x = t,
        None => s.print_templates.push(t),
    }
    s.save_prefs()?;
    Ok(json!({"userTemplates": user_names(s)}))
}

/// Renames a user template; the new name must be free (ignoring case).
fn rename_template(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.renameTemplate";
    let name = name_key(p, "name", ID)?;
    let to = name_key(p, "to", ID)?;
    check_user_name(ID, to)?;
    if s.print_templates.iter().any(|t| t.name.eq_ignore_ascii_case(to) && !t.name.eq_ignore_ascii_case(name)) {
        return Err(bad(ID, format!("a print template named `{to}` already exists")));
    }
    let t = s
        .print_templates
        .iter_mut()
        .find(|t| t.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| bad(ID, format!("no user print template `{name}`")))?;
    t.name = to.to_string();
    s.save_prefs()?;
    Ok(json!({"userTemplates": user_names(s)}))
}

/// Removes a user template; built-in templates stay.
fn delete_template(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.deleteTemplate";
    let name = name_key(p, "name", ID)?;
    let before = s.print_templates.len();
    s.print_templates.retain(|t| !t.name.eq_ignore_ascii_case(name));
    if s.print_templates.len() == before {
        return Err(bad(ID, format!("no user print template `{name}`")));
    }
    s.save_prefs()?;
    Ok(json!({"userTemplates": user_names(s)}))
}

fn render(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.render";
    let index = index_param(p, ID, "page")?;
    let model = s.print.clone();
    let ppi = ppi_param(p, ID, model.job.ppi)?;
    let photos = s.targets(p)?;
    let pages = print::plan(&model, photos.len()).map_err(|e| bad(ID, e))?.len();
    let img = print::render_page(s, &model, &photos, index, ppi).map_err(|e| bad(ID, e))?;
    let mut out = json!({"page": index, "pages": pages, "ppi": ppi, "width": img.width, "height": img.height});
    if let Some(path) = path_param(p, "path") {
        let bytes = encode_image(&img, &slideshow::jpeg_options()).map_err(|e| bad(ID, e))?;
        s.check_write_target(path).map_err(|e| bad(ID, e))?;
        write_file(path, &bytes).map_err(|e| bad(ID, e))?;
        out["path"] = json!(path);
        out["bytes"] = json!(bytes.len());
    }
    Ok(out)
}

/// The activity-stack row for a print export: its progress, and its ✕ stops the export before the
/// next page.
fn print_task(s: &Session) -> TaskGuard {
    s.activity.start("export", "Printing", Cancel::Yes)
}

fn export(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.export";
    let photos = s.targets(p)?;
    let model = s.print.clone();
    let ppi = ppi_param(p, ID, model.job.ppi)?;
    let pages = print::plan(&model, photos.len()).map_err(|e| bad(ID, e))?.len();
    let format = p.get("format").and_then(Value::as_str).unwrap_or(model.job.output.as_str());
    match format {
        "printer" => Err(bad(ID, "printing to a printer needs the OS print API, which this build does not have: print to PDF or JPEG")),
        "pdf" => {
            let path = path_param(p, "path").ok_or_else(|| bad(ID, "missing `path`"))?;
            let task = print_task(s);
            let mut progress = |done: usize, total: usize| {
                task.progress(done as u64, total as u64);
                !task.is_cancelled()
            };
            let bytes = print::export_pdf_with(s, &model, &photos, ppi, &mut progress).map_err(|e| bad(ID, e))?;
            s.check_write_target(path).map_err(|e| bad(ID, e))?;
            write_file(path, &bytes).map_err(|e| bad(ID, e))?;
            Ok(json!({"format": "pdf", "path": path, "pages": pages, "ppi": ppi, "bytes": bytes.len()}))
        }
        "jpeg" | "jpegs" => {
            let dir = path_param(p, "dir").ok_or_else(|| bad(ID, "missing `dir`"))?;
            let task = print_task(s);
            let mut progress = |done: usize, total: usize| {
                task.progress(done as u64, total as u64);
                !task.is_cancelled()
            };
            let files = print::export_jpegs_with(s, &model, &photos, ppi, &mut progress).map_err(|e| bad(ID, e))?;
            let mut names = Vec::with_capacity(files.len());
            for (name, bytes) in &files {
                let path = std::path::Path::new(dir).join(name).to_string_lossy().to_string();
                s.check_write_target(&path).map_err(|e| bad(ID, e))?;
                write_file(&path, bytes).map_err(|e| bad(ID, e))?;
                names.push(name.clone());
            }
            Ok(json!({"format": "jpeg", "dir": dir, "files": names, "ppi": ppi}))
        }
        _ => Err(bad(ID, "format must be pdf or jpeg")),
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "print.get", "Print Settings", [], None,
            "{} → {model, templates: [built-in name], userTemplates: [name]}; the model has sections layout (style single|contact|package|custom, paper a4|letter, landscape, columns, rows, marginMm, spacingMm, cellWidthMm, cellHeightMm), image (zoomToFill, rotateToFit, repeatOne, strokePt, strokeColor), guides (rulers, dimensions, bleedMm, show; drawn over the print view only, never on the output), package (cells: [{widthMm, heightMm}]), page (identityPlate, infoText, captions, textSizePt, background), job (ppi, sharpen, sharpenAmount, output pdf|jpeg|printer, profile, intent)",
            always, get),
        cmd!(
            "print.set",
            "Set Print Settings",
            [],
            None,
            "{layout?, image?, guides?, page?, job?: sections holding only the settings to change; package?: [{widthMm, heightMm}] (replaces the cells)} → {model}",
            always,
            set
        ),
        cmd!(
            "print.applyTemplate",
            "Apply Print Template",
            [],
            None,
            "{name: a built-in (Contact Sheet|Single Image|Picture Package|Custom Grid) or a user template} → sets layout, image and package; the job and page options stay → {model}",
            always,
            apply_template
        ),
        cmd!(
            "print.saveTemplate",
            "Save Print Template",
            [],
            None,
            "{name} — saves the current layout, image and package as a user template (kept with the library preferences); replaces a user template of that name; built-in names are refused → {userTemplates}",
            always,
            save_template
        ),
        cmd!(
            "print.renameTemplate",
            "Rename Print Template",
            [],
            None,
            "{name, to} — renames a user template; built-in names and names in use are refused → {userTemplates}",
            always,
            rename_template
        ),
        cmd!(
            "print.deleteTemplate",
            "Delete Print Template",
            [],
            None,
            "{name} — removes a user template; built-in templates can't be deleted → {userTemplates}",
            always,
            delete_template
        ),
        cmd!(query "print.render", "Render Print Page", [], None,
            "{page, ppi?: 72–600 (the job's ppi), ids?: photo ids (default selection), path?: write the page as a JPEG there} → {page, pages, ppi, width, height, path?, bytes?}",
            always, render),
        cmd!(query "print.export", "Export Print", [], None,
            "{format?: pdf|jpeg (the job's output), path (pdf), dir (jpeg), ppi?: 72–600 (the job's ppi), ids?: photo ids} → a multi-page PDF (sRGB profile embedded) or one JPEG per page (print-0001.jpg, ...); progress and cancel show in the activity stack; printer output is refused",
            always, export),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_settings_render_and_export_through_commands() {
        let mut s = Session::with_demo();
        let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
        s.execute("print.set", &json!({"layout": {"columns": 1, "rows": 2}})).unwrap();
        assert!(s.execute("print.set", &json!({"layout": {"columns": 9}})).is_err());
        assert_eq!(s.print.layout.columns, 1, "a refused patch changes nothing");
        s.execute("print.applyTemplate", &json!({"name": "Picture Package"})).unwrap();
        assert_eq!(s.print.layout.style, "package");
        assert!(s.execute("print.applyTemplate", &json!({"name": "Poster"})).is_err());
        s.execute("print.applyTemplate", &json!({"name": "Contact Sheet"})).unwrap();
        let r = s.execute("print.render", &json!({"page": 0, "ppi": 72, "ids": ids})).unwrap();
        assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(595), Some(842)));
        assert_eq!(r["pages"], 1);
        assert!(s.execute("print.render", &json!({"page": 5, "ppi": 72, "ids": ids})).is_err());
        assert!(s.execute("print.export", &json!({"format": "printer", "ids": ids, "path": "x.pdf"})).is_err());
        assert!(s.execute("print.export", &json!({"format": "pdf", "ids": ids})).is_err(), "a PDF needs a path");
        assert!(s.execute("print.export", &json!({"format": "tiff", "ids": ids, "dir": "x"})).is_err());
        let g = s.execute("print.get", &json!({})).unwrap();
        assert!(g["templates"].as_array().unwrap().iter().any(|t| t == "Custom Grid"));
        assert_eq!(g["userTemplates"], json!([]));
    }

    #[test]
    fn the_job_resolution_is_the_default_export_resolution() {
        let mut s = Session::with_demo();
        let ids: Vec<u64> = s.catalog.photos().take(1).map(|p| p.id.0).collect();
        s.execute("print.set", &json!({"layout": {"style": "single"}, "job": {"ppi": 72}})).unwrap();
        let path = std::env::temp_dir().join(format!("print-job-ppi-{}.pdf", std::process::id()));
        let r = s.execute("print.export", &json!({"format": "pdf", "ids": ids, "path": path.to_string_lossy()})).unwrap();
        assert_eq!(r["ppi"], 72.0, "the export ignored the job's resolution");
        let r = s.execute("print.export", &json!({"format": "pdf", "ids": ids, "path": path.to_string_lossy(), "ppi": 150})).unwrap();
        assert_eq!(r["ppi"], 150.0, "an explicit ppi wins");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn user_print_templates_save_rename_apply_and_delete() {
        let mut s = Session::with_demo();
        s.execute("print.applyTemplate", &json!({"name": "Contact Sheet"})).unwrap();
        s.execute("print.set", &json!({"layout": {"columns": 2, "rows": 5}, "image": {"zoomToFill": true}})).unwrap();
        let saved = s.execute("print.saveTemplate", &json!({"name": "Mine"})).unwrap();
        assert_eq!(saved["userTemplates"], json!(["Mine"]));
        assert!(s.execute("print.saveTemplate", &json!({"name": "Single Image"})).is_err(), "built-in names are reserved");
        assert!(s.execute("print.saveTemplate", &json!({"name": "  "})).is_err());
        s.execute("print.set", &json!({"layout": {"columns": 1, "rows": 1}, "image": {"zoomToFill": false}})).unwrap();
        s.execute("print.applyTemplate", &json!({"name": "mine"})).unwrap();
        assert_eq!((s.print.layout.columns, s.print.layout.rows, s.print.image.zoom_to_fill), (2, 5, true));
        // applying keeps the job and page options
        s.execute("print.set", &json!({"job": {"ppi": 150}, "page": {"infoText": "x"}})).unwrap();
        s.execute("print.applyTemplate", &json!({"name": "Single Image"})).unwrap();
        assert_eq!((s.print.job.ppi, s.print.page.info_text.as_str(), s.print.layout.style.as_str()), (150.0, "x", "single"));
        s.execute("print.applyTemplate", &json!({"name": "Mine"})).unwrap();
        s.execute("print.renameTemplate", &json!({"name": "Mine", "to": "Wedding"})).unwrap();
        assert!(s.execute("print.applyTemplate", &json!({"name": "Mine"})).is_err(), "the old name is gone");
        assert!(s.execute("print.renameTemplate", &json!({"name": "Wedding", "to": "Contact Sheet"})).is_err());
        assert!(s.execute("print.renameTemplate", &json!({"name": "Nope", "to": "X"})).is_err());
        s.execute("print.applyTemplate", &json!({"name": "wedding"})).unwrap();
        assert_eq!(s.print.layout.columns, 2);
        let g = s.execute("print.get", &json!({})).unwrap();
        assert_eq!(g["userTemplates"], json!(["Wedding"]));
        assert!(s.execute("print.deleteTemplate", &json!({"name": "Contact Sheet"})).is_err(), "built-in templates stay");
        s.execute("print.deleteTemplate", &json!({"name": "Wedding"})).unwrap();
        assert!(s.print_templates.is_empty());
        assert!(s.execute("print.applyTemplate", &json!({"name": "Wedding"})).is_err());
    }
}
