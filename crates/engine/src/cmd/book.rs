//! `book.*`: the Classic Book settings, pages, auto layout, page rendering and export (see
//! [`crate::book`]). The desktop app's page view is UI (`book.view`). Every change is saved with the
//! library preferences (`prefs.json`).

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};
use crate::book::{self, Model, Page};
use crate::export::{encode_image, write_file};
use crate::slideshow;
use crate::{Result, Session};

fn layouts_json() -> Value {
    Value::Array(
        book::builtin_layouts()
            .into_iter()
            .map(|l| {
                let cells: Vec<Value> = l.cells.iter().map(|c| json!({"x": c.x, "y": c.y, "w": c.w, "h": c.h})).collect();
                json!({"name": l.name, "cells": cells})
            })
            .collect(),
    )
}

fn get(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(json!({"model": s.book, "layouts": layouts_json(), "pages": s.book.pages.len()}))
}

fn index_param(p: &Value, id: &str, key: &str) -> Result<usize> {
    let i = p.get(key).and_then(Value::as_u64).ok_or_else(|| bad(id, format!("missing `{key}`")))?;
    usize::try_from(i).map_err(|_| bad(id, format!("`{key}` is out of range")))
}

fn dpi_param(p: &Value, id: &str) -> Result<f64> {
    match p.get("dpi") {
        None | Some(Value::Null) => Ok(book::DEFAULT_DPI),
        Some(v) => v.as_f64().ok_or_else(|| bad(id, "dpi must be a number")),
    }
}

fn path_param<'a>(p: &'a Value, key: &str) -> Option<&'a str> {
    p.get(key).and_then(Value::as_str).filter(|x| !x.trim().is_empty())
}

fn rgb(v: &Value) -> std::result::Result<[u8; 3], String> {
    let a = v.as_array().filter(|a| a.len() == 3).ok_or("a colour is [r, g, b] with values 0–255")?;
    let mut out = [0u8; 3];
    for (o, c) in out.iter_mut().zip(a) {
        *o = c.as_u64().and_then(|n| u8::try_from(n).ok()).ok_or("a colour is [r, g, b] with values 0–255")?;
    }
    Ok(out)
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "book.set";
    s.book = book::merged(&s.book, p).map_err(|e| bad(ID, e))?;
    s.save_prefs()?;
    Ok(json!({"model": s.book}))
}

fn add_page(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "book.addPage";
    let template = p.get("template").and_then(Value::as_str).unwrap_or("Single");
    let mut m = s.book.clone();
    let pages = book::add_page(&mut m, template).map_err(|e| bad(ID, e))?;
    s.book = m;
    s.save_prefs()?;
    Ok(json!({"pages": pages, "index": pages - 1}))
}

fn remove_page(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "book.removePage";
    let index = index_param(p, ID, "index")?;
    let mut m = s.book.clone();
    book::remove_page(&mut m, index).map_err(|e| bad(ID, e))?;
    s.book = m;
    s.save_prefs()?;
    Ok(json!({"pages": s.book.pages.len()}))
}

fn set_page(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "book.setPage";
    let index = index_param(p, ID, "index")?;
    let mut m = s.book.clone();
    let page: &mut Page = m.pages.get_mut(index).ok_or_else(|| bad(ID, format!("no page {index}")))?;
    if let Some(t) = p.get("template") {
        let name = t.as_str().ok_or_else(|| bad(ID, "template must be a name"))?;
        book::set_template(page, name).map_err(|e| bad(ID, e))?;
    }
    if let Some(t) = p.get("text") {
        page.text = t.as_str().ok_or_else(|| bad(ID, "text must be a string"))?.to_string();
    }
    if let Some(b) = p.get("background") {
        let b = b.as_object().ok_or_else(|| bad(ID, "background must be an object"))?;
        for (k, v) in b {
            match k.as_str() {
                "color" if v.is_null() => page.background.color = None,
                "color" => page.background.color = Some(rgb(v).map_err(|e| bad(ID, e))?),
                "photo" if v.is_null() => page.background.photo = None,
                "photo" => page.background.photo = Some(v.as_u64().ok_or_else(|| bad(ID, "photo must be a photo id or null"))?),
                "opacity" => page.background.opacity = v.as_f64().ok_or_else(|| bad(ID, "opacity must be a number"))?,
                _ => return Err(bad(ID, format!("unknown background setting `{k}`"))),
            }
        }
    }
    m.validate().map_err(|e| bad(ID, e))?;
    s.book = m;
    s.save_prefs()?;
    let page = &s.book.pages[index];
    Ok(json!({"index": index, "page": page}))
}

fn set_cell(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "book.setCell";
    let index = index_param(p, ID, "page")?;
    let cell = index_param(p, ID, "cell")?;
    let photo = match p.get("photo") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_u64().ok_or_else(|| bad(ID, "photo must be a photo id or null"))?),
    };
    let mut m = s.book.clone();
    let page = m.pages.get_mut(index).ok_or_else(|| bad(ID, format!("no page {index}")))?;
    book::set_cell(page, cell, photo).map_err(|e| bad(ID, e))?;
    s.book = m;
    s.save_prefs()?;
    Ok(json!({"page": index, "cell": cell, "photo": photo}))
}

fn auto_layout(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "book.autoLayout";
    let template = p.get("template").and_then(Value::as_str).ok_or_else(|| bad(ID, "missing `template`"))?;
    let replace = p.get("replace").and_then(Value::as_bool).unwrap_or(true);
    let ids = s.targets(p)?;
    let mut m = s.book.clone();
    let added = book::auto_layout(&mut m, template, &ids, replace).map_err(|e| bad(ID, e))?;
    s.book = m;
    s.save_prefs()?;
    Ok(json!({"pages": s.book.pages.len(), "added": added, "photos": ids.len()}))
}

fn render(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "book.render";
    let index = index_param(p, ID, "page")?;
    let dpi = dpi_param(p, ID)?;
    let model: Model = s.book.clone();
    let img = book::render_page(s, &model, index, dpi).map_err(|e| bad(ID, e))?;
    let mut out = json!({"page": index, "dpi": dpi, "width": img.width, "height": img.height});
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
    const ID: &str = "book.export";
    let format = p.get("format").and_then(Value::as_str).unwrap_or("pdf");
    let dpi = dpi_param(p, ID)?;
    let model: Model = s.book.clone();
    match format {
        "pdf" => {
            let path = path_param(p, "path").ok_or_else(|| bad(ID, "missing `path`"))?;
            let bytes = book::export_pdf(s, &model, dpi).map_err(|e| bad(ID, e))?;
            s.check_write_target(path).map_err(|e| bad(ID, e))?;
            write_file(path, &bytes).map_err(|e| bad(ID, e))?;
            Ok(json!({"format": "pdf", "path": path, "pages": model.pages.len(), "dpi": dpi, "bytes": bytes.len()}))
        }
        "jpegs" => {
            let dir = path_param(p, "dir").ok_or_else(|| bad(ID, "missing `dir`"))?;
            let files = book::export_jpegs(s, &model, dpi).map_err(|e| bad(ID, e))?;
            let mut names = Vec::with_capacity(files.len());
            for (name, bytes) in &files {
                let path = std::path::Path::new(dir).join(name).to_string_lossy().to_string();
                s.check_write_target(&path).map_err(|e| bad(ID, e))?;
                write_file(&path, bytes).map_err(|e| bad(ID, e))?;
                names.push(name.clone());
            }
            Ok(json!({"format": "jpegs", "dir": dir, "files": names, "dpi": dpi}))
        }
        _ => Err(bad(ID, "format must be pdf or jpegs")),
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "book.get", "Book Settings", [], None,
            "{} → {model, layouts: [{name, cells: [{x, y, w, h}]}], pages}; the model has sections settings (widthMm, heightMm, marginMm, cellPaddingMm, paperColor, colourSpace, cover), typography, text, guides, pages",
            always, get),
        cmd!(
            "book.set",
            "Set Book Settings",
            [],
            None,
            "{settings?: {widthMm, heightMm, marginMm, cellPaddingMm, paperColor, cover: {enabled, title}}, typography?: {font, sizePt, align, color}, text?: {photo}, guides?: {bleedMm, safeMm, show}} — each section holds only the settings to change → {model}",
            always,
            set
        ),
        cmd!(
            "book.addPage",
            "Add Book Page",
            [],
            None,
            "{template?: Single|Two Across|Two Stacked|Four Grid|Six Grid (Single)} → {pages, index}",
            always,
            add_page
        ),
        cmd!("book.removePage", "Remove Book Page", [], None, "{index} → {pages}", always, remove_page),
        cmd!(
            "book.setPage",
            "Set Book Page",
            [],
            None,
            "{index, template?: a template name (photos keep their cells), text?: page text (fields of its first photo in {braces}), background?: {color?: [r,g,b] | null, photo?: photo id | null, opacity?: 0–1}} → {index, page}",
            always,
            set_page
        ),
        cmd!(
            "book.setCell",
            "Set Book Cell",
            [],
            None,
            "{page, cell, photo: photo id | null} — puts a photo in a cell, or empties it → {page, cell, photo}",
            always,
            set_cell
        ),
        cmd!(
            "book.autoLayout",
            "Auto Layout Book",
            [],
            None,
            "{template: Single|Two Across|Two Stacked|Four Grid|Six Grid, replace?: true (clears the pages first; false appends), ids?: photo ids (default selection)} → {pages, added, photos}",
            always,
            auto_layout
        ),
        cmd!(query "book.render", "Render Book Page", [], None,
            "{page, dpi?: 36–600 (150), path?: write the page as a JPEG there} → {page, dpi, width, height, path?, bytes?}",
            always, render),
        cmd!(query "book.export", "Export Book", [], None,
            "{format?: pdf|jpegs (pdf), path (pdf), dir (jpegs), dpi?: 36–600 (150)} → a multi-page PDF or one JPEG per page (page-0001.jpg, ...); replaces an existing output, never an original",
            always, export),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_settings_pages_and_export_through_commands() {
        let mut s = Session::with_demo();
        let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
        // a patch changes one setting; a typo or a bad value is refused and nothing changes
        s.execute("book.set", &json!({"settings": {"widthMm": 200.0}})).unwrap();
        assert!(s.execute("book.set", &json!({"settings": {"widthMm": 5.0}})).is_err());
        assert!(s.execute("book.set", &json!({"settings": {"widthMm": 220.0, "marginMm": -1.0}})).is_err());
        assert_eq!(s.book.settings.width_mm, 200.0);
        // auto layout fills two pages of two cells from the photos given
        let auto = s.execute("book.autoLayout", &json!({"template": "Two Across", "ids": ids})).unwrap();
        assert_eq!((auto["pages"].as_u64(), auto["added"].as_u64()), (Some(2), Some(2)));
        assert!(s.execute("book.autoLayout", &json!({"template": "Poster", "ids": ids})).is_err());
        assert_eq!(s.book.pages.len(), 2);
        // page text and background; a bad opacity is refused and the page is unchanged
        s.execute("book.setPage", &json!({"index": 0, "text": "{file}"})).unwrap();
        s.execute("book.setPage", &json!({"index": 1, "background": {"color": [20, 20, 20], "photo": ids[0], "opacity": 0.5}})).unwrap();
        assert!(s.execute("book.setPage", &json!({"index": 1, "background": {"opacity": 2.0}})).is_err());
        assert_eq!(s.book.pages[1].background.opacity, 0.5);
        assert!(s.execute("book.setPage", &json!({"index": 1, "background": {"colour": [1, 2, 3]}})).is_err());
        // a cell takes a photo or empties; a template change keeps the photos that still have a cell
        s.execute("book.setCell", &json!({"page": 1, "cell": 1, "photo": ids[2]})).unwrap();
        assert_eq!(s.book.pages[1].photos, vec![Some(ids[2]), Some(ids[2])]);
        s.execute("book.setCell", &json!({"page": 1, "cell": 0, "photo": null})).unwrap();
        assert!(s.execute("book.setCell", &json!({"page": 1, "cell": 5, "photo": null})).is_err());
        s.execute("book.setPage", &json!({"index": 1, "template": "Four Grid"})).unwrap();
        assert_eq!(s.book.pages[1].photos.len(), 4);
        s.execute("book.setPage", &json!({"index": 1, "template": "Two Across"})).unwrap();
        // pages are added and removed
        assert_eq!(s.execute("book.addPage", &json!({"template": "Six Grid"})).unwrap()["pages"], 3);
        s.execute("book.removePage", &json!({"index": 2})).unwrap();
        assert!(s.execute("book.removePage", &json!({"index": 2})).is_err());
        // rendering and export
        let r = s.execute("book.render", &json!({"page": 0, "dpi": 72})).unwrap();
        assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(567), Some(595)));
        assert!(s.execute("book.render", &json!({"page": 0, "dpi": 1000})).is_err());
        assert!(s.execute("book.export", &json!({"format": "pdf", "dpi": 72})).is_err(), "a PDF needs a path");
        assert!(s.execute("book.export", &json!({"format": "tiff", "dir": "x"})).is_err());
        let g = s.execute("book.get", &json!({})).unwrap();
        assert_eq!(g["pages"], 2);
        assert!(g["layouts"].as_array().unwrap().iter().any(|l| l["name"] == "Six Grid" && l["cells"].as_array().unwrap().len() == 6));
    }
}
