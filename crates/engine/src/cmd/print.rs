//! `print.*`: the Classic Print job, its layout, pages and export (see [`crate::print`]). The print
//! view is UI (`print.view`).

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};
use crate::export::{encode_image, write_file};
use crate::print;
use crate::slideshow;
use crate::{Result, Session};

fn get(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(json!({"model": s.print, "templates": print::builtin_templates()}))
}

fn index_param(p: &Value, id: &str, key: &str) -> Result<usize> {
    let i = p.get(key).and_then(Value::as_u64).ok_or_else(|| bad(id, format!("missing `{key}`")))?;
    usize::try_from(i).map_err(|_| bad(id, format!("`{key}` is out of range")))
}

fn ppi_param(p: &Value, id: &str) -> Result<f64> {
    match p.get("ppi") {
        None | Some(Value::Null) => Ok(print::DEFAULT_PPI),
        Some(v) => v.as_f64().ok_or_else(|| bad(id, "ppi must be a number")),
    }
}

fn path_param<'a>(p: &'a Value, key: &str) -> Option<&'a str> {
    p.get(key).and_then(Value::as_str).filter(|x| !x.trim().is_empty())
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.set";
    s.print = print::merged(&s.print, p).map_err(|e| bad(ID, e))?;
    Ok(json!({"model": s.print}))
}

fn apply_template(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.applyTemplate";
    let name = p.get("name").and_then(Value::as_str).ok_or_else(|| bad(ID, "missing `name`"))?;
    let t = print::template(name).map_err(|e| bad(ID, e))?;
    s.print.layout = t.layout;
    s.print.image = t.image;
    s.print.package = t.package;
    Ok(json!({"model": s.print}))
}

fn render(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.render";
    let index = index_param(p, ID, "page")?;
    let ppi = ppi_param(p, ID)?;
    let photos = s.targets(p)?;
    let model = s.print.clone();
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

fn export(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.export";
    let photos = s.targets(p)?;
    let model = s.print.clone();
    let ppi = ppi_param(p, ID)?;
    let pages = print::plan(&model, photos.len()).map_err(|e| bad(ID, e))?.len();
    let format = p.get("format").and_then(Value::as_str).unwrap_or(model.job.output.as_str());
    match format {
        "printer" => Err(bad(ID, "printing to a printer needs the OS print API, which this build does not have: print to PDF or JPEG")),
        "pdf" => {
            let path = path_param(p, "path").ok_or_else(|| bad(ID, "missing `path`"))?;
            let bytes = print::export_pdf(s, &model, &photos, ppi).map_err(|e| bad(ID, e))?;
            s.check_write_target(path).map_err(|e| bad(ID, e))?;
            write_file(path, &bytes).map_err(|e| bad(ID, e))?;
            Ok(json!({"format": "pdf", "path": path, "pages": pages, "ppi": ppi, "bytes": bytes.len()}))
        }
        "jpeg" | "jpegs" => {
            let dir = path_param(p, "dir").ok_or_else(|| bad(ID, "missing `dir`"))?;
            let files = print::export_jpegs(s, &model, &photos, ppi).map_err(|e| bad(ID, e))?;
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
            "{} → {model, templates: [name]}; the model has sections layout (style single|contact|package|custom, paper a4|letter, landscape, columns, rows, marginMm, spacingMm, cellWidthMm, cellHeightMm), image (zoomToFill, rotateToFit, repeatOne, strokePt, strokeColor), guides (rulers, dimensions, bleedMm, show), package (cells: [{widthMm, heightMm}]), page (identityPlate, infoText, captions, textSizePt, background), job (ppi, sharpen, sharpenAmount, output pdf|jpeg|printer, profile, intent)",
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
            "{name: Contact Sheet|Single Image|Picture Package|Custom Grid} → sets layout, image and package; the job and page options stay → {model}",
            always,
            apply_template
        ),
        cmd!(query "print.render", "Render Print Page", [], None,
            "{page, ppi?: 72–600 (300), ids?: photo ids (default selection), path?: write the page as a JPEG there} → {page, pages, ppi, width, height, path?, bytes?}",
            always, render),
        cmd!(query "print.export", "Export Print", [], None,
            "{format?: pdf|jpeg (the job's output), path (pdf), dir (jpeg), ppi?: 72–600 (300), ids?: photo ids} → a multi-page PDF or one JPEG per page (print-0001.jpg, ...); printer output is refused",
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
    }
}
