//! `slideshow.*`: the Classic Slideshow settings, templates, timeline, frames and export
//! (see [`crate::slideshow`]). The desktop app's playback view is UI (`slideshow.play`).

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};
use crate::export::{encode_image, write_file};
use crate::slideshow::{self, Slide, Template, Timeline};
use crate::{Result, Session};

/// Built-in templates followed by the user's, each with whether it is built in.
fn templates_json(s: &Session) -> Value {
    let builtin = slideshow::builtin_templates().into_iter().map(|t| json!({"name": t.name, "builtin": true, "model": t.model}));
    let user = s.slideshow_templates.iter().map(|t| json!({"name": t.name, "builtin": false, "model": t.model}));
    Value::Array(builtin.chain(user).collect())
}

fn slide_json(slide: Slide) -> Value {
    match slide {
        Slide::Intro => json!({"kind": "intro"}),
        Slide::Photo(id) => json!({"kind": "photo", "photo": id.0}),
        Slide::Ending => json!({"kind": "ending"}),
    }
}

fn name_param<'a>(p: &'a Value, cmd_id: &str) -> Result<&'a str> {
    p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(cmd_id, "missing `name`"))
}

fn width_param(p: &Value) -> usize {
    p.get("width").and_then(Value::as_u64).unwrap_or(1920) as usize
}

fn get(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(json!({"model": s.slideshow, "templates": templates_json(s)}))
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "slideshow.set";
    s.slideshow = slideshow::merged(&s.slideshow, p).map_err(|e| bad(ID, e))?;
    Ok(json!({"model": s.slideshow}))
}

fn save_template(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "slideshow.saveTemplate";
    let name = name_param(p, ID)?;
    if slideshow::builtin_templates().iter().any(|t| t.name.eq_ignore_ascii_case(name)) {
        return Err(bad(ID, format!("`{name}` is a built-in template; choose another name")));
    }
    let model = s.slideshow.clone();
    match s.slideshow_templates.iter_mut().find(|t| t.name.eq_ignore_ascii_case(name)) {
        Some(t) => t.model = model,
        None => s.slideshow_templates.push(Template { name: name.to_string(), model }),
    }
    s.save_prefs()?;
    Ok(json!({"templates": templates_json(s)}))
}

fn apply_template(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "slideshow.applyTemplate";
    let name = name_param(p, ID)?;
    s.slideshow = slideshow::find_template(&s.slideshow_templates, name).ok_or_else(|| bad(ID, format!("no template `{name}`")))?;
    Ok(json!({"model": s.slideshow}))
}

fn delete_template(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "slideshow.deleteTemplate";
    let name = name_param(p, ID)?;
    let before = s.slideshow_templates.len();
    s.slideshow_templates.retain(|t| !t.name.eq_ignore_ascii_case(name));
    if s.slideshow_templates.len() == before {
        return Err(bad(ID, format!("no user template `{name}`")));
    }
    s.save_prefs()?;
    Ok(json!({"templates": templates_json(s)}))
}

fn timeline(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = s.targets(p)?;
    let t = Timeline::new(&s.slideshow, &ids);
    let slides: Vec<Value> = t
        .entries
        .iter()
        .map(|e| {
            let mut v = slide_json(e.slide);
            v["start"] = json!(e.start);
            v["seconds"] = json!(e.seconds);
            v
        })
        .collect();
    Ok(json!({"duration": t.total, "fade": t.fade, "repeat": t.repeat, "slides": slides}))
}

fn frame(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "slideshow.frame";
    let t = p.get("t").and_then(Value::as_f64).unwrap_or(0.0);
    let (w, h) = slideshow::frame_size(&s.slideshow, width_param(p)).map_err(|e| bad(ID, e))?;
    let ids = s.targets(p)?;
    let model = s.slideshow.clone();
    let timeline = Timeline::new(&model, &ids);
    let (frame, img) =
        slideshow::render_frame(s, &model, &timeline, t, w, h).map_err(|e| bad(ID, e))?.ok_or_else(|| bad(ID, "no slide at that time"))?;
    let mut out = json!({
        "current": frame.current,
        "photo": timeline.slide(frame.current).and_then(|sl| if let Slide::Photo(id) = sl { Some(id.0) } else { None }),
        "previous": frame.previous,
        "alpha": frame.alpha,
        "width": w,
        "height": h,
    });
    if let Some(path) = p.get("path").and_then(Value::as_str).filter(|x| !x.trim().is_empty()) {
        let bytes = encode_image(&img, &slideshow::jpeg_options()).map_err(|e| bad(ID, e))?;
        s.check_write_target(path).map_err(|e| bad(ID, e))?;
        write_file(path, &bytes).map_err(|e| bad(ID, e))?;
        out["path"] = json!(path);
        out["bytes"] = json!(bytes.len());
    }
    Ok(out)
}

fn export(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "slideshow.export";
    let format = p.get("format").and_then(Value::as_str).unwrap_or("pdf");
    let width = width_param(p);
    let ids = s.targets(p)?;
    let model = s.slideshow.clone();
    let timeline = Timeline::new(&model, &ids);
    match format {
        "pdf" => {
            let path = p.get("path").and_then(Value::as_str).filter(|x| !x.trim().is_empty()).ok_or_else(|| bad(ID, "missing `path`"))?;
            let bytes = slideshow::export_pdf(s, &model, &timeline, width).map_err(|e| bad(ID, e))?;
            s.check_write_target(path).map_err(|e| bad(ID, e))?;
            write_file(path, &bytes).map_err(|e| bad(ID, e))?;
            Ok(json!({"format": "pdf", "path": path, "slides": timeline.entries.len(), "bytes": bytes.len()}))
        }
        "jpegs" => {
            let dir = p.get("dir").and_then(Value::as_str).filter(|x| !x.trim().is_empty()).ok_or_else(|| bad(ID, "missing `dir`"))?;
            let files = slideshow::export_jpegs(s, &model, &timeline, width).map_err(|e| bad(ID, e))?;
            let mut names = Vec::with_capacity(files.len());
            for (name, bytes) in &files {
                let path = std::path::Path::new(dir).join(name).to_string_lossy().to_string();
                s.check_write_target(&path).map_err(|e| bad(ID, e))?;
                write_file(&path, bytes).map_err(|e| bad(ID, e))?;
                names.push(name.clone());
            }
            Ok(json!({"format": "jpegs", "dir": dir, "files": names}))
        }
        _ => Err(bad(ID, "format must be pdf or jpegs")),
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "slideshow.get", "Slideshow Settings", [], None,
            "{} → {model, templates: [{name, builtin, model}]}; the model has sections layout, options, backdrop, overlays, titles, playback",
            always, get),
        cmd!(
            "slideshow.set",
            "Set Slideshow Settings",
            [],
            None,
            "{layout?: {aspect, margin}, options?: {strokeWidth, strokeColor, shadow, zoomToFill}, backdrop?: {color, gradientTo}, overlays?: {identityPlate, text, ratingStars, size, color}, titles?: {intro, ending, seconds, size, color}, playback?: {slideSeconds, fadeSeconds, random, repeat, seed}} — each section holds only the settings to change → {model}",
            always,
            set
        ),
        cmd!(
            "slideshow.saveTemplate",
            "Save Slideshow Template",
            [],
            None,
            "{name} — saves the current settings as a user template (kept with the library) → {templates}",
            always,
            save_template
        ),
        cmd!(
            "slideshow.applyTemplate",
            "Apply Slideshow Template",
            [],
            None,
            "{name} — makes a built-in or user template the current settings → {model}",
            always,
            apply_template
        ),
        cmd!(
            "slideshow.deleteTemplate",
            "Delete Slideshow Template",
            [],
            None,
            "{name} — removes a user template → {templates}",
            always,
            delete_template
        ),
        cmd!(query "slideshow.timeline", "Slideshow Timeline", [], None,
            "{ids?: photo ids (default selection)} → {duration, fade, repeat, slides: [{kind: intro|photo|ending, photo?, start, seconds}]}",
            always, timeline),
        cmd!(query "slideshow.frame", "Slideshow Frame", [], None,
            "{t: seconds into the show, width?: long edge 64–4096 (1920), ids?, path?: write the frame as a JPEG there} → {current, photo?, previous, alpha, width, height, path?, bytes?}; alpha is how far the current slide has faded in over the previous one (1 = settled)",
            always, frame),
        cmd!(query "slideshow.export", "Export Slideshow", [], None,
            "{format?: pdf|jpegs (pdf), path (pdf), dir (jpegs), width?: long edge 64–4096 (1920), ids?} → one page or one JPEG per slide, each slide settled; replaces an existing output, never an original",
            always, export),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_templates_and_timeline_through_commands() {
        let mut s = Session::with_demo();
        let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
        // a patch changes one setting; a typo is refused
        let m = s.execute("slideshow.set", &json!({"options": {"shadow": true}})).unwrap();
        assert_eq!(m["model"]["options"]["shadow"], true);
        assert!(s.execute("slideshow.set", &json!({"options": {"shadw": true}})).is_err());
        // save, change, apply: the saved settings come back
        s.execute("slideshow.saveTemplate", &json!({"name": "Mine"})).unwrap();
        s.execute("slideshow.set", &json!({"options": {"shadow": false}})).unwrap();
        let applied = s.execute("slideshow.applyTemplate", &json!({"name": "mine"})).unwrap();
        assert_eq!(applied["model"]["options"]["shadow"], true);
        // built-ins cannot be overwritten or deleted, user templates can
        assert!(s.execute("slideshow.saveTemplate", &json!({"name": "Framed"})).is_err());
        assert!(s.execute("slideshow.deleteTemplate", &json!({"name": "Framed"})).is_err());
        let all = s.execute("slideshow.get", &json!({})).unwrap();
        assert!(all["templates"].as_array().unwrap().iter().any(|t| t["name"] == "Mine" && t["builtin"] == false));
        s.execute("slideshow.deleteTemplate", &json!({"name": "Mine"})).unwrap();
        assert!(s.execute("slideshow.applyTemplate", &json!({"name": "Mine"})).is_err());
        // the timeline lists the photos in order with a title screen first
        s.execute("slideshow.set", &json!({"titles": {"intro": "Trip"}, "playback": {"slideSeconds": 2.0, "fadeSeconds": 0.5}})).unwrap();
        let tl = s.execute("slideshow.timeline", &json!({"ids": ids})).unwrap();
        let slides = tl["slides"].as_array().unwrap();
        assert_eq!(slides.len(), 4);
        assert_eq!(slides[0]["kind"], "intro");
        assert_eq!(slides[1]["photo"], ids[0]);
        assert_eq!(slides[1]["start"], 3.0);
        assert_eq!(tl["duration"], 9.0);
        // at 3.25 s the first photo is fading in over the title screen (slide 0)
        let f = s.execute("slideshow.frame", &json!({"ids": ids, "t": 3.25, "width": 256})).unwrap();
        assert_eq!((f["current"].as_u64(), f["previous"].as_u64()), (Some(1), Some(0)));
        assert_eq!(f["photo"], ids[0]);
        assert_eq!(f["width"], 256);
        assert!(s.execute("slideshow.frame", &json!({"ids": ids, "t": 99.0})).is_err());
    }
}
