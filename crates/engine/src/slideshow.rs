//! Classic Slideshow: the settings model, templates, playback timing, frame rendering and export.
//!
//! Every size is a fraction of the frame's short edge, so a frame at any resolution looks the same
//! (resolution independence). A slide is a photo rendered through the normal export pipeline
//! ([`Session::export_job`]) and placed on a backdrop with its overlays. Export writes each slide in
//! its settled state: a PDF with one JPEG page per slide, or a sequence of JPEG files.
use crate::Session;
use crate::contact_sheet::{Pdf, blit};
use crate::export::{Anchor, ExportFormat, ExportOptions, Watermark, draw_watermark, encode_image};
use lightcraft_catalog::{Photo, PhotoId};
use lightcraft_pipeline::{OutputDepth, OutputSpace};
use lightcraft_raster::Rgba8;
use serde::{Deserialize, Serialize};

/// Most slides one export writes (each is rendered and encoded in turn).
pub const MAX_SLIDES: usize = 500;
/// Longest text a template field may hold, in characters.
const MAX_TEXT: usize = 500;

/// The whole slideshow: persisted with templates, edited through `slideshow.set`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Model {
    pub layout: Layout,
    pub options: Options,
    pub backdrop: Backdrop,
    pub overlays: Overlays,
    pub titles: Titles,
    pub playback: Playback,
    pub music: Music,
}

/// Frame shape and the empty margin around each photo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Layout {
    /// Frame width ÷ height (0.2–5).
    pub aspect: f64,
    /// Margin around the photo, as a fraction of the short edge (0–0.25).
    pub margin: f64,
    /// Draw the margin lines on the module's preview. Guides are never drawn into a slide or an export.
    pub guides: bool,
}

impl Default for Layout {
    fn default() -> Self {
        Self { aspect: 16.0 / 9.0, margin: 0.05, guides: false }
    }
}

/// How each photo sits in its margin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Options {
    /// Border around the photo, as a fraction of the short edge (0–0.05); 0 = none.
    pub stroke_width: f64,
    pub stroke_color: [u8; 3],
    /// A soft dark shadow behind the photo.
    pub shadow: bool,
    /// Scale the photo to cover its area (cropping the overflow) instead of fitting inside it.
    pub zoom_to_fill: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { stroke_width: 0.0, stroke_color: [255, 255, 255], shadow: false, zoom_to_fill: false }
    }
}

/// The colour behind the photos: solid, or a vertical gradient from `color` to `gradient_to`. A catalog
/// photo (`image`, its id) covers the backdrop instead when it is set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Backdrop {
    pub color: [u8; 3],
    pub gradient_to: Option<[u8; 3]>,
    pub image: Option<u64>,
}

/// Text drawn over each photo. Templates may use `{file}`, `{title}`, `{caption}`, `{copyright}`,
/// `{city}`, `{country}`, `{camera}`, `{date}` and `{rating}`; other braces stay as written.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Overlays {
    /// Identity plate: text in the top-left corner.
    pub identity_plate: String,
    /// Text in the bottom-left corner.
    pub text: String,
    /// Rating stars (five, filled to the photo's rating) in the bottom-right corner.
    pub rating_stars: bool,
    /// Overlay text height, as a fraction of the short edge (0.005–0.1).
    pub size: f64,
    pub color: [u8; 3],
}

impl Default for Overlays {
    fn default() -> Self {
        Self { identity_plate: String::new(), text: String::new(), rating_stars: false, size: 0.03, color: [255, 255, 255] }
    }
}

/// Title screens shown before the first photo and after the last one (empty text = no screen).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Titles {
    pub intro: String,
    pub ending: String,
    /// How long each title screen stays (0.5–60 s).
    pub seconds: f64,
    /// Title text height, as a fraction of the short edge (0.01–0.2).
    pub size: f64,
    pub color: [u8; 3],
}

impl Default for Titles {
    fn default() -> Self {
        Self { intro: String::new(), ending: String::new(), seconds: 3.0, size: 0.06, color: [255, 255, 255] }
    }
}

/// Timing and order. A photo is shown for `slide_seconds`; each slide fades in over the first
/// `fade_seconds` of its time, over the slide before it (or over the backdrop for the first one).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Playback {
    /// 0.5–60 s per photo.
    pub slide_seconds: f64,
    /// Cross-fade length, 0 to `slide_seconds`.
    pub fade_seconds: f64,
    /// Shuffle the photos (the order follows `seed`).
    pub random: bool,
    /// Start again after the last slide.
    pub repeat: bool,
    /// Advance by itself when the show plays; `false` (manual) moves only when stepped.
    pub auto: bool,
    pub seed: u64,
}

impl Default for Playback {
    fn default() -> Self {
        Self { slide_seconds: 3.0, fade_seconds: 1.0, random: false, repeat: false, auto: true, seed: 1 }
    }
}

/// The soundtrack the show is timed to: a file path. The engine does not decode audio; the length
/// the slides are fitted to is given by the caller (see [`fit_slide_seconds`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Music {
    pub path: String,
}

/// The seconds each photo shows so that the title screens and the photos fill `music_seconds`.
/// Errors when there are no photos, the music length is not positive, or the photos would not fit
/// at 0.5–60 seconds each.
pub fn fit_slide_seconds(music_seconds: f64, photos: usize, title_seconds: f64, titles: usize) -> Result<f64, String> {
    if photos == 0 {
        return Err("select photos to fit to the music".into());
    }
    if !music_seconds.is_finite() || music_seconds <= 0.0 {
        return Err("the music length must be a positive number of seconds".into());
    }
    let per = (music_seconds - title_seconds * titles as f64) / photos as f64;
    if !(0.5..=60.0).contains(&per) {
        return Err(format!("{photos} photos cannot fill {music_seconds:.1} s of music at {per:.2} s each (0.5–60 s per photo)"));
    }
    Ok(per)
}

/// A named set of settings: built-in ones ([`builtin_templates`]) or the user's, saved with the library.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Template {
    pub name: String,
    pub model: Model,
}

/// The built-in templates.
pub fn builtin_templates() -> Vec<Template> {
    let mut white = Model { backdrop: Backdrop { color: [255, 255, 255], ..Backdrop::default() }, ..Model::default() };
    white.overlays.color = [32, 32, 32];
    white.titles.color = [32, 32, 32];
    let framed = Model {
        options: Options { stroke_width: 0.012, stroke_color: [240, 240, 240], shadow: true, zoom_to_fill: false },
        backdrop: Backdrop { color: [40, 40, 44], gradient_to: Some([8, 8, 10]), ..Backdrop::default() },
        ..Model::default()
    };
    vec![
        Template { name: "Black".into(), model: Model::default() },
        Template { name: "White".into(), model: white },
        Template { name: "Framed".into(), model: framed },
    ]
}

/// The settings a name refers to: a built-in template first, then the user's.
pub fn find_template(user: &[Template], name: &str) -> Option<Model> {
    let name = name.trim();
    builtin_templates().into_iter().chain(user.iter().cloned()).find(|t| t.name.eq_ignore_ascii_case(name)).map(|t| t.model)
}

impl Model {
    /// Refuses values outside the ranges the renderer supports.
    pub fn validate(&self) -> Result<(), String> {
        fn check(ok: bool, msg: &str) -> Result<(), String> {
            if ok { Ok(()) } else { Err(msg.to_string()) }
        }
        let (l, o, v, t, p) = (&self.layout, &self.options, &self.overlays, &self.titles, &self.playback);
        check((0.2..=5.0).contains(&l.aspect), "layout.aspect must be 0.2–5 (width ÷ height)")?;
        check((0.0..=0.25).contains(&l.margin), "layout.margin must be 0–0.25 of the short edge")?;
        check((0.0..=0.05).contains(&o.stroke_width), "options.strokeWidth must be 0–0.05 of the short edge")?;
        check((0.005..=0.1).contains(&v.size), "overlays.size must be 0.005–0.1 of the short edge")?;
        check((0.01..=0.2).contains(&t.size), "titles.size must be 0.01–0.2 of the short edge")?;
        check((0.5..=60.0).contains(&t.seconds), "titles.seconds must be 0.5–60")?;
        check((0.5..=60.0).contains(&p.slide_seconds), "playback.slideSeconds must be 0.5–60")?;
        check((0.0..=p.slide_seconds).contains(&p.fade_seconds), "playback.fadeSeconds must be 0 up to slideSeconds")?;
        for text in [&v.identity_plate, &v.text, &t.intro, &t.ending] {
            check(text.chars().count() <= MAX_TEXT, "text must be at most 500 characters")?;
        }
        check(self.music.path.chars().count() <= 4096, "music.path must be at most 4096 characters")?;
        Ok(())
    }
}

/// The settings with `patch` applied. A patch is an object of sections (`layout`, `options`,
/// `backdrop`, `overlays`, `titles`, `playback`), each holding only the settings to change; a
/// name that is not a setting is refused.
pub fn merged(current: &Model, patch: &serde_json::Value) -> Result<Model, String> {
    let serde_json::Value::Object(sections) = patch else { return Err("settings must be an object of sections".into()) };
    let mut v = serde_json::to_value(current).map_err(|e| e.to_string())?;
    for (section, values) in sections {
        let serde_json::Value::Object(values) = values else { return Err(format!("`{section}` must be an object")) };
        let target = v.get_mut(section).and_then(serde_json::Value::as_object_mut).ok_or_else(|| format!("no settings section `{section}`"))?;
        for (key, value) in values {
            if !target.contains_key(key) {
                return Err(format!("unknown setting `{section}.{key}`"));
            }
            target.insert(key.clone(), value.clone());
        }
    }
    let model: Model = serde_json::from_value(v).map_err(|e| e.to_string())?;
    model.validate()?;
    Ok(model)
}

/// Frame size in pixels for a long edge of `long_edge` pixels at the model's aspect.
pub fn frame_size(model: &Model, long_edge: usize) -> Result<(usize, usize), String> {
    model.validate()?;
    if !(64..=4096).contains(&long_edge) {
        return Err("width must be 64–4096 pixels".into());
    }
    let a = model.layout.aspect;
    let (w, h) = if a >= 1.0 { (long_edge, (long_edge as f64 / a).round() as usize) } else { ((long_edge as f64 * a).round() as usize, long_edge) };
    Ok((w.max(16), h.max(16)))
}

/// One thing on screen: a title screen or a photo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Slide {
    Intro,
    Photo(PhotoId),
    Ending,
}

/// A slide with its place on the timeline, in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Entry {
    pub slide: Slide,
    pub start: f64,
    pub seconds: f64,
}

/// The slides in play order with their start times.
#[derive(Clone, Debug, PartialEq)]
pub struct Timeline {
    pub entries: Vec<Entry>,
    /// Cross-fade length in seconds (never longer than the shortest slide).
    pub fade: f64,
    pub repeat: bool,
    /// Length of one pass through the slides, in seconds.
    pub total: f64,
}

/// What is on screen at one time: slide `current` (index into the timeline), blended over slide
/// `previous` (`None` = over the backdrop) with weight `alpha` (1 = `current` alone).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub current: usize,
    pub previous: Option<usize>,
    pub alpha: f64,
}

impl Timeline {
    /// The slides for `ids` in the order given (shuffled with the playback seed when random).
    pub fn new(model: &Model, ids: &[PhotoId]) -> Timeline {
        let mut order = ids.to_vec();
        if model.playback.random {
            shuffle(&mut order, model.playback.seed);
        }
        let mut slides = Vec::with_capacity(order.len() + 2);
        if !model.titles.intro.trim().is_empty() {
            slides.push((Slide::Intro, model.titles.seconds));
        }
        slides.extend(order.into_iter().map(|id| (Slide::Photo(id), model.playback.slide_seconds)));
        if !model.titles.ending.trim().is_empty() {
            slides.push((Slide::Ending, model.titles.seconds));
        }
        let mut start = 0.0;
        let entries: Vec<Entry> = slides
            .into_iter()
            .map(|(slide, seconds)| {
                let entry = Entry { slide, start, seconds };
                start += seconds;
                entry
            })
            .collect();
        let shortest = entries.iter().map(|e| e.seconds).fold(f64::INFINITY, f64::min);
        let fade = if entries.is_empty() { 0.0 } else { model.playback.fade_seconds.clamp(0.0, shortest) };
        Timeline { entries, fade, repeat: model.playback.repeat, total: start }
    }

    /// The slide at `index`, when there is one.
    pub fn slide(&self, index: usize) -> Option<Slide> {
        self.entries.get(index).map(|e| e.slide)
    }

    /// When slide `index` starts (0 when there is no such slide).
    pub fn start_of(&self, index: usize) -> f64 {
        self.entries.get(index).map_or(0.0, |e| e.start)
    }

    /// What is on screen at `t` seconds. Without `repeat`, `None` from the end of the timeline on.
    pub fn frame_at(&self, t: f64) -> Option<Frame> {
        if self.entries.is_empty() || self.total.is_nan() || self.total <= 0.0 {
            return None;
        }
        let t = if t.is_finite() && t > 0.0 { t } else { 0.0 };
        if !self.repeat && t >= self.total {
            return None;
        }
        let loops = if self.repeat { (t / self.total).floor() } else { 0.0 };
        let local_t = t - loops * self.total;
        let last = self.entries.len() - 1;
        let index = self.entries.iter().position(|e| local_t < e.start + e.seconds).unwrap_or(last);
        let entry = self.entries.get(index)?;
        let local = (local_t - entry.start).max(0.0);
        let alpha = if self.fade > 0.0 { (local / self.fade).min(1.0) } else { 1.0 };
        let previous = if index > 0 {
            Some(index - 1)
        } else if loops > 0.0 && last > 0 {
            // a repeat fades the first slide in from the last one
            Some(last)
        } else {
            None
        };
        Some(Frame { current: index, previous, alpha })
    }
}

/// Deterministic Fisher–Yates shuffle with splitmix64 (same seed, same order).
fn shuffle(order: &mut [PhotoId], seed: u64) {
    let mut state = seed;
    let mut next = move || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    for i in (1..order.len()).rev() {
        let j = (next() % (i as u64 + 1)) as usize;
        order.swap(i, j);
    }
}

/// The photo facts an overlay template can name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fields {
    pub file: String,
    pub title: String,
    pub caption: String,
    pub copyright: String,
    pub city: String,
    pub country: String,
    pub camera: String,
    pub date: String,
    pub rating: u8,
}

impl Fields {
    pub fn of(p: &Photo) -> Self {
        Self {
            file: p.file_name.clone(),
            title: p.meta.title.clone(),
            caption: p.meta.caption.clone(),
            copyright: p.meta.copyright.clone(),
            city: p.meta.city.clone(),
            country: p.meta.country.clone(),
            camera: p.meta.camera.clone(),
            date: p.captured.as_deref().and_then(|c| c.split('T').next()).unwrap_or("").to_string(),
            rating: p.rating,
        }
    }

    fn field(&self, key: &str) -> Option<String> {
        Some(match key {
            "file" => self.file.clone(),
            "title" => self.title.clone(),
            "caption" => self.caption.clone(),
            "copyright" => self.copyright.clone(),
            "city" => self.city.clone(),
            "country" => self.country.clone(),
            "camera" => self.camera.clone(),
            "date" => self.date.clone(),
            "rating" => self.rating.to_string(),
            _ => return None,
        })
    }
}

/// `template` with its `{field}` names replaced by the photo's values. Unknown names stay as written.
pub fn expand(template: &str, fields: &Fields) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        let Some(close) = after.find('}') else {
            out.push_str(after);
            return out;
        };
        let key = &after[1..close];
        match fields.field(key) {
            Some(value) => out.push_str(&value),
            None => out.push_str(&after[..=close]),
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// The backdrop at `w` × `h`: the backdrop photo covering the frame when one is set, else the colour.
pub fn backdrop_for(session: &mut Session, model: &Model, w: usize, h: usize) -> Result<Rgba8, String> {
    let Some(id) = model.backdrop.image else { return Ok(backdrop(model, w, h)) };
    let job = session.export_job(PhotoId(id), w, h, OutputSpace::Srgb, OutputDepth::U8)?;
    let photo = job.run().rendered?.image;
    Ok(cover(&photo, w, h))
}

/// A plain backdrop: solid, or a vertical gradient from the colour to `gradient_to`.
pub fn backdrop(model: &Model, w: usize, h: usize) -> Rgba8 {
    let top = model.backdrop.color;
    let bottom = model.backdrop.gradient_to.unwrap_or(top);
    let mut img = Rgba8::filled(w, h, [0, 0, 0, 255]);
    for y in 0..h {
        let t = if h > 1 { y as f64 / (h - 1) as f64 } else { 0.0 };
        let c = |i: usize| (f64::from(top[i]) + (f64::from(bottom[i]) - f64::from(top[i])) * t).round().clamp(0.0, 255.0) as u8;
        img.row_mut(y).fill([c(0), c(1), c(2), 255]);
    }
    img
}

/// One slide at `w` × `h`, settled (no fade applied).
pub fn render_slide(session: &mut Session, model: &Model, slide: Slide, w: usize, h: usize) -> Result<Rgba8, String> {
    let mut img = backdrop_for(session, model, w, h)?;
    match slide {
        Slide::Photo(id) => {
            place_photo(session, model, id, &mut img)?;
            let fields = session.catalog.photo(id).map(|p| Fields::of(p)).ok_or("no such photo")?;
            draw_overlays(model, &fields, &mut img);
        }
        Slide::Intro => draw_title(model, &model.titles.intro, &mut img),
        Slide::Ending => draw_title(model, &model.titles.ending, &mut img),
    }
    Ok(img)
}

/// `a` and `b` mixed: `alpha` of `b` over `a`.
pub fn blend(a: &Rgba8, b: &Rgba8, alpha: f64) -> Rgba8 {
    if a.width != b.width || a.height != b.height {
        return b.clone();
    }
    let k = (alpha.clamp(0.0, 1.0) * 255.0).round() as u32;
    let ik = 255 - k;
    let mix = |p: u8, q: u8| ((u32::from(p) * ik + u32::from(q) * k + 127) / 255) as u8;
    let mut out = b.clone();
    for (o, (x, y)) in out.data.iter_mut().zip(a.data.iter().zip(b.data.iter())) {
        *o = [mix(x[0], y[0]), mix(x[1], y[1]), mix(x[2], y[2]), 255];
    }
    out
}

/// The frame at `t` seconds with what it shows, or `None` past the end of a timeline that does not repeat.
pub fn render_frame(session: &mut Session, model: &Model, timeline: &Timeline, t: f64, w: usize, h: usize) -> Result<Option<(Frame, Rgba8)>, String> {
    let Some(frame) = timeline.frame_at(t) else { return Ok(None) };
    let slide = timeline.slide(frame.current).ok_or("no slide at that time")?;
    let current = render_slide(session, model, slide, w, h)?;
    if frame.alpha >= 1.0 {
        return Ok(Some((frame, current)));
    }
    let from = match frame.previous.and_then(|p| timeline.slide(p)) {
        Some(prev) => render_slide(session, model, prev, w, h)?,
        None => backdrop_for(session, model, w, h)?,
    };
    Ok(Some((frame, blend(&from, &current, frame.alpha))))
}

/// The PDF of the timeline: one page per slide, each a JPEG at `long_edge` pixels on a 150 dpi page.
pub fn export_pdf(session: &mut Session, model: &Model, timeline: &Timeline, long_edge: usize) -> Result<Vec<u8>, String> {
    let (w, h) = frame_size(model, long_edge)?;
    check_slide_count(timeline)?;
    let points = (w as f64 * 72.0 / 150.0, h as f64 * 72.0 / 150.0);
    let mut pdf = Pdf::new(timeline.entries.len(), points);
    for (i, entry) in timeline.entries.iter().enumerate() {
        let img = render_slide(session, model, entry.slide, w, h)?;
        let jpg = encode_image(&img, &jpeg_options())?;
        pdf.page(i, &jpg, w, h);
    }
    Ok(pdf.finish())
}

/// One JPEG per slide, named `slide-0001.jpg`, `slide-0002.jpg`, … in play order.
pub fn export_jpegs(session: &mut Session, model: &Model, timeline: &Timeline, long_edge: usize) -> Result<Vec<(String, Vec<u8>)>, String> {
    let (w, h) = frame_size(model, long_edge)?;
    check_slide_count(timeline)?;
    let mut files = Vec::with_capacity(timeline.entries.len());
    for (i, entry) in timeline.entries.iter().enumerate() {
        let img = render_slide(session, model, entry.slide, w, h)?;
        files.push((format!("slide-{:04}.jpg", i + 1), encode_image(&img, &jpeg_options())?));
    }
    Ok(files)
}

fn check_slide_count(timeline: &Timeline) -> Result<(), String> {
    if timeline.entries.is_empty() {
        return Err("no slides: select photos or add a title".into());
    }
    if timeline.entries.len() > MAX_SLIDES {
        return Err(format!("slideshows export at most {MAX_SLIDES} slides; select fewer photos"));
    }
    Ok(())
}

/// The JPEG encoding slides are exported with.
pub fn jpeg_options() -> ExportOptions {
    ExportOptions { format: ExportFormat::Jpeg, quality: 92, ..Default::default() }
}

/// The photo fitted (or covering) its area inside the margin and stroke, with the shadow and border.
fn place_photo(session: &mut Session, model: &Model, id: PhotoId, img: &mut Rgba8) -> Result<(), String> {
    let (w, h) = (img.width, img.height);
    let short = w.min(h) as f64;
    let margin = (model.layout.margin * short).round();
    let stroke = (model.options.stroke_width * short).round();
    let inset = margin + stroke;
    let pw = (w as f64 - 2.0 * inset).round().max(1.0) as usize;
    let ph = (h as f64 - 2.0 * inset).round().max(1.0) as usize;
    let job = session.export_job(id, pw, ph, OutputSpace::Srgb, OutputDepth::U8)?;
    let mut photo = job.run().rendered?.image;
    if model.options.zoom_to_fill {
        photo = cover(&photo, pw, ph);
    }
    let frame_w = pw + 2 * stroke as usize;
    let frame_h = ph + 2 * stroke as usize;
    if model.options.shadow {
        let d = (short * 0.01).max(2.0).round() as usize;
        darken(img, margin as usize + d, margin as usize + d, frame_w, frame_h);
    }
    if stroke > 0.0 {
        fill_rect(img, margin as usize, margin as usize, frame_w, frame_h, model.options.stroke_color);
    }
    let x = inset as usize + (pw - photo.width.min(pw)) / 2;
    let y = inset as usize + (ph - photo.height.min(ph)) / 2;
    blit(img, &photo, x, y)
}

/// Scale `img` to cover `w` × `h` (nearest sample), centred, cropping the overflow.
pub(crate) fn cover(img: &Rgba8, w: usize, h: usize) -> Rgba8 {
    let mut out = Rgba8::filled(w, h, [0, 0, 0, 255]);
    if img.width == 0 || img.height == 0 {
        return out;
    }
    let s = (w as f64 / img.width as f64).max(h as f64 / img.height as f64);
    let scaled_w = (img.width as f64 * s).ceil().max(w as f64);
    let scaled_h = (img.height as f64 * s).ceil().max(h as f64);
    let ox = (scaled_w - w as f64) / 2.0;
    let oy = (scaled_h - h as f64) / 2.0;
    for y in 0..h {
        for x in 0..w {
            let sx = (((x as f64 + ox + 0.5) / s) as usize).min(img.width - 1);
            let sy = (((y as f64 + oy + 0.5) / s) as usize).min(img.height - 1);
            let px = img.data.get(sy * img.width + sx).copied().unwrap_or([0, 0, 0, 255]);
            if let Some(d) = out.data.get_mut(y * w + x) {
                *d = px;
            }
        }
    }
    out
}

fn fill_rect(img: &mut Rgba8, x: usize, y: usize, w: usize, h: usize, color: [u8; 3]) {
    let (xs, ys) = (clamp_range(x as f64, (x + w) as f64, img.width), clamp_range(y as f64, (y + h) as f64, img.height));
    for py in ys {
        for px in xs.clone() {
            if let Some(p) = img.data.get_mut(py * img.width + px) {
                *p = [color[0], color[1], color[2], 255];
            }
        }
    }
}

fn darken(img: &mut Rgba8, x: usize, y: usize, w: usize, h: usize) {
    let (xs, ys) = (clamp_range(x as f64, (x + w) as f64, img.width), clamp_range(y as f64, (y + h) as f64, img.height));
    for py in ys {
        for px in xs.clone() {
            if let Some(p) = img.data.get_mut(py * img.width + px) {
                for c in p.iter_mut().take(3) {
                    *c = (f32::from(*c) * 0.45) as u8;
                }
            }
        }
    }
}

/// `lo..hi` clamped to `0..max`, as indices.
fn clamp_range(lo: f64, hi: f64, max: usize) -> std::ops::Range<usize> {
    let a = lo.floor().clamp(0.0, max as f64) as usize;
    let b = hi.ceil().clamp(0.0, max as f64) as usize;
    a..b.max(a)
}

fn text_overlay(text: String, anchor: Anchor, inset: f64, size: f64, color: [u8; 3]) -> Watermark {
    Watermark {
        text,
        size: size as f32,
        opacity: 1.0,
        anchor,
        inset_x: inset as f32,
        inset_y: inset as f32,
        color,
        shadow: true,
        ..Default::default()
    }
}

fn draw_overlays(model: &Model, fields: &Fields, img: &mut Rgba8) {
    let o = &model.overlays;
    let short = img.width.min(img.height) as f64;
    let inset = model.layout.margin;
    let identity = expand(&o.identity_plate, fields);
    if !identity.trim().is_empty() {
        draw_watermark(img, &text_overlay(identity, Anchor::TopLeft, inset, o.size, o.color));
    }
    let text = expand(&o.text, fields);
    if !text.trim().is_empty() {
        draw_watermark(img, &text_overlay(text, Anchor::BottomLeft, inset, o.size, o.color));
    }
    if o.rating_stars {
        draw_stars(img, fields.rating.min(5), inset * short, o.size * short * 0.5, o.color);
    }
}

/// Five stars along the bottom-right corner: the first `filled` in `color`, the rest grey.
fn draw_stars(img: &mut Rgba8, filled: u8, inset: f64, r: f64, color: [u8; 3]) {
    let spacing = 2.4 * r;
    let total = 5.0 * spacing - 0.4 * r;
    let start_x = img.width as f64 - inset - total;
    let cy = img.height as f64 - inset - r;
    for i in 0..5usize {
        let cx = start_x + r + i as f64 * spacing;
        let c = if i < usize::from(filled) { color } else { [120, 120, 120] };
        let points: Vec<(f64, f64)> = (0..10)
            .map(|k| {
                let radius = if k % 2 == 0 { r } else { 0.45 * r };
                let angle = -std::f64::consts::FRAC_PI_2 + k as f64 * std::f64::consts::PI / 5.0;
                (cx + radius * angle.cos(), cy + radius * angle.sin())
            })
            .collect();
        fill_polygon(img, &points, c);
    }
}

fn fill_polygon(img: &mut Rgba8, pts: &[(f64, f64)], color: [u8; 3]) {
    let min_x = pts.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let max_x = pts.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
    let min_y = pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let max_y = pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    let (xs, ys) = (clamp_range(min_x, max_x, img.width), clamp_range(min_y, max_y, img.height));
    for py in ys {
        for px in xs.clone() {
            let (x, y) = (px as f64 + 0.5, py as f64 + 0.5);
            // even-odd crossing test
            let mut inside = false;
            let mut j = pts.len().saturating_sub(1);
            for (i, &(xi, yi)) in pts.iter().enumerate() {
                let (xj, yj) = pts.get(j).copied().unwrap_or((xi, yi));
                if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                    inside = !inside;
                }
                j = i;
            }
            if inside && let Some(p) = img.data.get_mut(py * img.width + px) {
                *p = [color[0], color[1], color[2], 255];
            }
        }
    }
}

fn draw_title(model: &Model, text: &str, img: &mut Rgba8) {
    let text = text.replace(['\n', '\r'], " ");
    if text.trim().is_empty() {
        return;
    }
    let t = &model.titles;
    draw_watermark(
        img,
        &Watermark { text, size: t.size as f32, opacity: 1.0, anchor: Anchor::Center, color: t.color, shadow: true, ..Default::default() },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ids(n: u64) -> Vec<PhotoId> {
        (1..=n).map(PhotoId).collect()
    }

    /// Three 3-second slides, a 1-second fade.
    fn three_slides() -> (Model, Timeline) {
        let model = Model::default();
        model.validate().unwrap();
        let mut m = model;
        m.playback.slide_seconds = 3.0;
        m.playback.fade_seconds = 1.0;
        let t = Timeline::new(&m, &ids(3));
        (m, t)
    }

    #[test]
    fn model_serde_defaults_and_partial_documents() {
        let empty: Model = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, Model::default());
        assert_eq!(empty.playback.slide_seconds, 3.0);
        // a partial document keeps every setting it does not name
        let partial: Model = serde_json::from_str(r#"{"playback": {"slideSeconds": 5.0}, "backdrop": {"color": [9, 9, 9]}}"#).unwrap();
        assert_eq!(partial.playback.slide_seconds, 5.0);
        assert_eq!(partial.playback.fade_seconds, 1.0);
        assert_eq!(partial.backdrop.color, [9, 9, 9]);
        assert_eq!(partial.backdrop.gradient_to, None);
        assert_eq!(partial.layout, Layout::default());
        // the newer settings default off, auto-advance on, no music
        assert!(!partial.layout.guides);
        assert_eq!(partial.backdrop.image, None);
        assert!(partial.playback.auto);
        assert!(partial.music.path.is_empty());
        // camelCase on disk
        let v = serde_json::to_value(&partial).unwrap();
        assert!(v["playback"].get("slideSeconds").is_some());
        assert!(v["layout"].get("margin").is_some());
        assert!(v["backdrop"].get("gradientTo").is_some());
        assert!(v["music"].get("path").is_some());
    }

    #[test]
    fn backdrop_colour_fills_the_corners_and_washes_to_the_second_colour() {
        let mut s = Session::with_demo();
        let id = s.catalog.photos().next().unwrap().id;
        let mut m = Model::default();
        m.backdrop.color = [12, 200, 40];
        m.layout.margin = 0.1;
        let img = render_slide(&mut s, &m, Slide::Photo(id), 480, 270).unwrap();
        // the photo sits inside the margins, so the corners show the backdrop colour
        assert_eq!(img.data[0], [12, 200, 40, 255], "top-left corner");
        assert_eq!(img.data[img.data.len() - 1], [12, 200, 40, 255], "bottom-right corner");
        // a colour wash runs from the first colour at the top to the second at the bottom
        let wash = Model { backdrop: Backdrop { color: [0, 0, 0], gradient_to: Some([200, 100, 0]), ..Backdrop::default() }, ..Model::default() };
        let strip = backdrop(&wash, 1, 5);
        assert_eq!(strip.data[0], [0, 0, 0, 255]);
        assert_eq!(strip.data[4], [200, 100, 0, 255]);
    }

    #[test]
    fn backdrop_photo_covers_the_frame_instead_of_the_colour() {
        let mut s = Session::with_demo();
        let id = s.catalog.photos().next().unwrap().id;
        let mut m = Model::default();
        m.backdrop.color = [1, 2, 3];
        m.backdrop.image = Some(id.0);
        let img = backdrop_for(&mut s, &m, 64, 36).unwrap();
        assert_eq!((img.width, img.height), (64, 36));
        assert!(img.data.iter().any(|p| *p != [1, 2, 3, 255]), "the photo is drawn");
        // with no image, the colour backdrop is used
        m.backdrop.image = None;
        assert_eq!(backdrop_for(&mut s, &m, 64, 36).unwrap(), backdrop(&m, 64, 36));
    }

    #[test]
    fn layout_guides_never_reach_a_slide_or_an_export() {
        // the guides are the module view's overlay (#131): switching them on changes no pixel or byte
        let mut s = Session::with_demo();
        let ids: Vec<PhotoId> = s.catalog.photos().take(2).map(|p| p.id).collect();
        let plain = Model::default();
        let guided = Model { layout: Layout { guides: true, ..Layout::default() }, ..Model::default() };
        assert!(!plain.layout.guides && guided.layout.guides, "the two settings differ in the guides only");
        assert_eq!(plain.layout.margin, guided.layout.margin);
        let off = render_slide(&mut s, &plain, Slide::Photo(ids[0]), 480, 270).unwrap();
        let on = render_slide(&mut s, &guided, Slide::Photo(ids[0]), 480, 270).unwrap();
        assert_eq!(off, on, "a slide renders the same with the guides on");
        let timeline = Timeline::new(&plain, &ids);
        let pdf_off = export_pdf(&mut s, &plain, &timeline, 256).unwrap();
        let pdf_on = export_pdf(&mut s, &guided, &timeline, 256).unwrap();
        assert_eq!(pdf_off, pdf_on, "the PDF is the same bytes");
        let jpegs_off = export_jpegs(&mut s, &plain, &timeline, 256).unwrap();
        let jpegs_on = export_jpegs(&mut s, &guided, &timeline, 256).unwrap();
        assert_eq!(jpegs_off, jpegs_on, "the JPEGs are the same bytes");
    }

    #[test]
    fn cover_crops_the_overflow_from_both_sides() {
        // left half red, right half blue, cover a square: the sides are cropped, the halves survive
        let mut src = Rgba8::filled(4, 2, [0, 0, 0, 255]);
        for y in 0..2 {
            for x in 0..4 {
                src.data[y * 4 + x] = if x < 2 { [255, 0, 0, 255] } else { [0, 0, 255, 255] };
            }
        }
        let out = cover(&src, 2, 2);
        assert_eq!(out.data[0], [255, 0, 0, 255]);
        assert_eq!(out.data[1], [0, 0, 255, 255]);
    }

    #[test]
    fn music_fits_the_slides_to_the_track() {
        // 30 s of music, one 2 s title screen, 8 photos: (30 - 2) / 8 = 3.5 s each
        assert_eq!(fit_slide_seconds(30.0, 8, 2.0, 1).unwrap(), 3.5);
        assert_eq!(fit_slide_seconds(12.0, 3, 0.0, 0).unwrap(), 4.0);
        // refused: no photos, a track the photos cannot fill at 0.5–60 s each, or a title longer than the track
        assert!(fit_slide_seconds(30.0, 0, 2.0, 1).is_err());
        assert!(fit_slide_seconds(1.0, 8, 2.0, 1).is_err(), "titles alone are longer");
        assert!(fit_slide_seconds(1000.0, 8, 2.0, 0).is_err(), "over 60 s each");
        assert!(fit_slide_seconds(f64::NAN, 8, 0.0, 0).is_err());
    }

    #[test]
    fn patches_change_only_named_settings_and_refuse_typos() {
        let base = Model::default();
        let m = merged(&base, &json!({"options": {"shadow": true}, "layout": {"aspect": 1.0}})).unwrap();
        assert!(m.options.shadow);
        assert_eq!(m.layout.aspect, 1.0);
        assert_eq!(m.layout.margin, base.layout.margin);
        assert_eq!(m.playback, base.playback);
        assert!(merged(&base, &json!({"options": {"shadw": true}})).is_err());
        assert!(merged(&base, &json!({"optons": {}})).is_err());
        assert!(merged(&base, &json!({"layout": {"aspect": 50.0}})).is_err());
        assert!(merged(&base, &json!({"playback": {"slideSeconds": 2.0, "fadeSeconds": 3.0}})).is_err());
        assert!(merged(&base, &json!(3)).is_err());
    }

    #[test]
    fn frame_timing_picks_the_photo_and_fade_alpha() {
        let (_, t) = three_slides();
        assert_eq!(t.total, 9.0);
        // the first slide fades in from the backdrop
        assert_eq!(t.frame_at(0.0), Some(Frame { current: 0, previous: None, alpha: 0.0 }));
        assert_eq!(t.frame_at(0.5), Some(Frame { current: 0, previous: None, alpha: 0.5 }));
        assert_eq!(t.frame_at(2.0), Some(Frame { current: 0, previous: None, alpha: 1.0 }));
        // the second slide starts at 3 s and fades over the first slide
        assert_eq!(t.frame_at(3.25), Some(Frame { current: 1, previous: Some(0), alpha: 0.25 }));
        assert_eq!(t.frame_at(4.5), Some(Frame { current: 1, previous: Some(0), alpha: 1.0 }));
        assert_eq!(t.frame_at(7.0), Some(Frame { current: 2, previous: Some(1), alpha: 1.0 }));
        assert_eq!(t.entries[2].slide, Slide::Photo(PhotoId(3)));
        // the end: nothing without repeat, the first slide again with it
        assert_eq!(t.frame_at(9.0), None);
        assert_eq!(t.frame_at(-4.0), t.frame_at(0.0));
        assert_eq!(t.frame_at(f64::NAN), t.frame_at(0.0));
    }

    #[test]
    fn repeat_wraps_and_fades_the_first_slide_from_the_last() {
        let mut m = Model::default();
        m.playback.repeat = true;
        let t = Timeline::new(&m, &ids(2));
        assert_eq!(t.frame_at(0.5).unwrap().previous, None, "the first pass starts over the backdrop");
        let wrapped = t.frame_at(6.5).unwrap();
        assert_eq!((wrapped.current, wrapped.previous), (0, Some(1)));
        assert!((wrapped.alpha - 0.5).abs() < 1e-9);
    }

    #[test]
    fn titles_are_slides_with_their_own_length_and_fade_is_capped() {
        let mut m = Model::default();
        m.titles.intro = "Holiday".into();
        m.titles.ending = "  ".into(); // blank text: no ending screen
        m.titles.seconds = 2.0;
        m.playback.slide_seconds = 1.0;
        m.playback.fade_seconds = 5.0; // capped at the shortest slide
        let t = Timeline::new(&m, &ids(2));
        assert_eq!(t.entries.iter().map(|e| e.slide).collect::<Vec<_>>(), vec![Slide::Intro, Slide::Photo(PhotoId(1)), Slide::Photo(PhotoId(2))]);
        assert_eq!(t.entries[1].start, 2.0);
        assert_eq!(t.fade, 1.0);
        assert!(Timeline::new(&m, &[]).frame_at(0.0).is_some(), "a title alone still plays");
        assert!(Timeline::new(&Model::default(), &[]).frame_at(0.0).is_none());
    }

    #[test]
    fn random_order_is_a_repeatable_permutation() {
        let mut m = Model::default();
        m.playback.random = true;
        m.playback.seed = 7;
        let a = Timeline::new(&m, &ids(12));
        let b = Timeline::new(&m, &ids(12));
        assert_eq!(a, b, "same seed, same order");
        let mut got: Vec<u64> = a.entries.iter().filter_map(|e| if let Slide::Photo(p) = e.slide { Some(p.0) } else { None }).collect();
        assert_ne!(got, (1..=12).collect::<Vec<_>>(), "shuffled");
        got.sort_unstable();
        assert_eq!(got, (1..=12).collect::<Vec<_>>(), "every photo once");
    }

    #[test]
    fn overlay_templates_fill_photo_fields() {
        let f = Fields { file: "IMG_0001.jpg".into(), title: "Harbour".into(), date: "2026-05-01".into(), rating: 4, ..Default::default() };
        assert_eq!(expand("{title} · {date} · {file}", &f), "Harbour · 2026-05-01 · IMG_0001.jpg");
        assert_eq!(expand("{rating}/5 {unknown} {", &f), "4/5 {unknown} {");
        assert_eq!(expand("plain text", &f), "plain text");
        assert_eq!(expand("{}{copyright}", &f), "{}");
    }

    #[test]
    fn template_round_trip_and_builtins() {
        let mut m = Model::default();
        m.overlays.identity_plate = "© Orthic".into();
        m.options.stroke_width = 0.01;
        m.backdrop.gradient_to = Some([1, 2, 3]);
        let t = Template { name: "Mine".into(), model: m.clone() };
        let json = serde_json::to_string(&t).unwrap();
        let back: Template = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
        let user = vec![t];
        assert_eq!(find_template(&user, "mine"), Some(m));
        assert_eq!(find_template(&user, "Framed").map(|f| f.options.shadow), Some(true));
        assert!(find_template(&user, "nope").is_none());
        for b in builtin_templates() {
            b.model.validate().unwrap();
        }
    }

    #[test]
    fn frame_size_follows_the_aspect() {
        let mut m = Model::default();
        assert_eq!(frame_size(&m, 1920).unwrap(), (1920, 1080));
        m.layout.aspect = 0.5;
        assert_eq!(frame_size(&m, 1000).unwrap(), (500, 1000));
        assert!(frame_size(&m, 32).is_err());
        assert!(frame_size(&m, 8192).is_err());
    }

    #[test]
    fn frames_render_the_slide_and_blend_it_over_its_neighbour() {
        let mut s = Session::with_demo();
        let photos: Vec<PhotoId> = s.catalog.photos().take(2).map(|p| p.id).collect();
        assert_eq!(photos.len(), 2);
        let mut m = Model::default();
        m.backdrop.color = [200, 10, 10];
        m.playback.fade_seconds = 1.0;
        let t = Timeline::new(&m, &photos);
        let (w, h) = frame_size(&m, 256).unwrap();
        // at t = 0 the first photo has not faded in: the frame is the backdrop
        let (f0, img0) = render_frame(&mut s, &m, &t, 0.0, w, h).unwrap().unwrap();
        assert_eq!((f0.current, f0.alpha), (0, 0.0));
        assert_eq!(img0, backdrop(&m, w, h));
        assert_eq!((img0.width, img0.height), (w, h));
        // a settled frame shows the photo, not the backdrop
        let (f1, img1) = render_frame(&mut s, &m, &t, 2.0, w, h).unwrap().unwrap();
        assert_eq!(f1.alpha, 1.0);
        assert_ne!(img1, backdrop(&m, w, h));
        // past the end: nothing
        assert!(render_frame(&mut s, &m, &t, 100.0, w, h).unwrap().is_none());
    }

    #[test]
    fn overlays_and_stroke_change_the_pixels() {
        let mut s = Session::with_demo();
        let id = s.catalog.photos().next().unwrap().id;
        let mut plain = Model::default();
        plain.backdrop.color = [0, 0, 0];
        let base = render_slide(&mut s, &plain, Slide::Photo(id), 480, 270).unwrap();
        let mut stars = plain.clone();
        stars.overlays.rating_stars = true;
        stars.overlays.text = "{file}".into();
        let with_text = render_slide(&mut s, &stars, Slide::Photo(id), 480, 270).unwrap();
        assert_ne!(with_text, base, "overlays drawn");
        let mut framed = plain.clone();
        framed.options.stroke_width = 0.04;
        framed.options.stroke_color = [255, 0, 0];
        let edge = render_slide(&mut s, &framed, Slide::Photo(id), 480, 270).unwrap();
        assert!(edge.data.iter().any(|p| p[0] == 255 && p[1] == 0 && p[2] == 0), "stroke drawn in its colour");
    }

    #[test]
    fn pdf_and_jpeg_sequence_have_one_page_per_slide() {
        let mut s = Session::with_demo();
        let photos: Vec<PhotoId> = s.catalog.photos().take(2).map(|p| p.id).collect();
        let mut m = Model::default();
        m.titles.intro = "Intro".into();
        let t = Timeline::new(&m, &photos);
        let pdf = export_pdf(&mut s, &m, &t, 256).unwrap();
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/Count 3 /Kids"));
        assert!(text.starts_with("%PDF-1.4"));
        let files = export_jpegs(&mut s, &m, &t, 256).unwrap();
        assert_eq!(files.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), vec!["slide-0001.jpg", "slide-0002.jpg", "slide-0003.jpg"]);
        assert!(files.iter().all(|f| f.1.starts_with(&[0xff, 0xd8])));
        assert!(export_pdf(&mut s, &m, &Timeline::new(&Model::default(), &[]), 256).is_err());
    }
}
