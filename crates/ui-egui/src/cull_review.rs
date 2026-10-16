//! Assisted Culling as a reviewed proposal (#44). Photo ▸ Assisted Culling starts the read-only
//! analysis as a cancellable background job with its own activity row. The result is a proposal
//! shown in pages of burst groups: each photo's suggested pick or reject, its reasons and whether
//! Apply will set it. Nothing is flagged until Apply, which re-measures the accepted photos off the
//! UI thread and commits their flags as one undo step. Cancelling or discarding before Apply leaves
//! the catalog untouched.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::Duration;

use lightcraft_engine::activity::{Cancel, TaskGuard};
use lightcraft_engine::cmd::cull::{CullApplyJob, CullJob, CullJobResult};
use serde_json::{Value, json};

use crate::LightcraftApp;

/// Burst groups (and the single shots with a suggestion) listed on one page.
const SECTIONS_PER_PAGE: usize = 6;

/// What a finished analysis sends back: the job (to validate its result against the catalog) and the outcome.
type AnalyzeOutcome = (CullJob, Result<CullJobResult, String>);
/// What a finished apply sends back: the prepared apply (to commit it) and the re-measurement.
type ApplyOutcome = (CullApplyJob, Result<CullJobResult, String>);

/// Where the Assisted Culling review is. `Idle` shows nothing.
#[derive(Default)]
pub enum Phase {
    #[default]
    Idle,
    Analyzing(Run<AnalyzeOutcome>),
    Review(Review),
    Applying(Run<ApplyOutcome>),
}

/// A background job's place: its result channel, its activity row and its cancel flag. Dropping
/// it removes the row.
pub struct Run<T> {
    rx: Receiver<T>,
    _guard: TaskGuard,
    cancel: Arc<AtomicBool>,
}

/// The app's review state (`LightcraftApp::cull_review`).
#[derive(Default)]
pub struct CullReview {
    pub phase: Phase,
}

/// A reviewed proposal: the engine's proposal (applied as it is, minus the suggestions the user
/// declined) and the rows and pages shown to the user.
pub struct Review {
    proposal: Value,
    pub rows: Vec<Row>,
    sections: Vec<Section>,
    pub page: usize,
}

/// One photo in the review. `proposed` is the engine's suggested flag (`pick` or `reject`);
/// `accept` says whether Apply sets it.
pub struct Row {
    pub id: u64,
    pub name: String,
    pub proposed: Option<String>,
    pub accept: bool,
    pub sharpness: f64,
    pub clipped: f64,
    pub reasons: Vec<String>,
}

/// A burst group (`Some`) or the single shots with a suggestion (`None`), by index into `Review::rows`.
struct Section {
    group: Option<u64>,
    rows: Vec<usize>,
}

/// What a button in the window asks for, carried out once the frame's drawing is done.
enum Action {
    None,
    Cancel,
    Apply,
    Discard,
    Page(usize),
}

impl Review {
    /// The review of an engine `photo.cullSuggest` result. Photos neither in a burst nor suggested
    /// are left out: they have nothing to review.
    fn new(value: &Value) -> Self {
        let proposal = value.get("proposal").cloned().unwrap_or(Value::Null);
        let mut rows = Vec::new();
        let mut groups: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
        let mut singles = Vec::new();
        for photo in proposal["photos"].as_array().into_iter().flatten() {
            let group = photo["group"].as_u64();
            let proposed = photo["proposedFlag"].as_str().map(str::to_owned);
            if group.is_none() && proposed.is_none() {
                continue;
            }
            let index = rows.len();
            match group {
                Some(group) => groups.entry(group).or_default().push(index),
                None => singles.push(index),
            }
            rows.push(Row {
                id: photo["id"].as_u64().unwrap_or_default(),
                name: photo["fileName"].as_str().unwrap_or_default().to_owned(),
                accept: proposed.is_some(),
                proposed,
                sharpness: photo["sharpness"].as_f64().unwrap_or_default(),
                clipped: photo["clipped"].as_f64().unwrap_or_default(),
                reasons: photo["reasonCodes"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect(),
            });
        }
        let mut sections: Vec<Section> = groups.into_iter().map(|(group, rows)| Section { group: Some(group), rows }).collect();
        if !singles.is_empty() {
            sections.push(Section { group: None, rows: singles });
        }
        Self { proposal, rows, sections, page: 0 }
    }

    /// At least one page, even when nothing is listed.
    fn pages(&self) -> usize {
        self.sections.len().div_ceil(SECTIONS_PER_PAGE).max(1)
    }

    fn suggested(&self) -> usize {
        self.rows.iter().filter(|row| row.proposed.is_some()).count()
    }

    fn accepted(&self) -> usize {
        self.rows.iter().filter(|row| row.accept && row.proposed.is_some()).count()
    }

    /// The sections on the current page, as (group, row indices).
    fn page_sections(&self) -> Vec<(Option<u64>, Vec<usize>)> {
        let first = self.page.saturating_mul(SECTIONS_PER_PAGE);
        self.sections.iter().skip(first).take(SECTIONS_PER_PAGE).map(|section| (section.group, section.rows.clone())).collect()
    }

    /// The params of `photo.cullApply`: the proposal as it was offered, and exactly the accepted suggestions.
    fn apply_params(&self) -> Value {
        let accept: Vec<Value> = self
            .rows
            .iter()
            .filter(|row| row.accept)
            .filter_map(|row| row.proposed.as_ref().map(|flag| json!({"id": row.id, "flag": flag})))
            .collect();
        json!({"proposal": self.proposal.clone(), "accept": accept})
    }
}

/// Runs `work` on a worker thread (inline on the web, which has none) and returns where its result arrives.
fn background<T: Send + 'static>(name: &str, work: impl FnOnce() -> T + Send + 'static) -> Result<Receiver<T>, String> {
    let (tx, rx) = channel();
    let run = move || {
        let _ = tx.send(work());
    };
    let _ = name;
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::Builder::new().name(name.into()).spawn(run).map_err(|error| error.to_string())?;
    #[cfg(target_arch = "wasm32")]
    run();
    Ok(rx)
}

/// Photo ▸ Assisted Culling: queue the read-only analysis of the selected photos (or those in view).
/// Nothing is flagged; the result is reviewed in the window before [`apply`] writes anything.
pub fn start(app: &mut LightcraftApp, reject_below: f32, pick_best: bool) -> Result<Value, String> {
    if !matches!(app.cull_review.phase, Phase::Idle) {
        return Err(crate::i18n::tr("Finish the open culling review first").to_string());
    }
    let ids = if app.session.selection.ids.len() > 1 { app.session.selected() } else { app.session.visible_cloned() };
    if ids.is_empty() {
        return Err(crate::i18n::tr("There are no photos to analyze").to_string());
    }
    let reject = (reject_below > 0.0).then_some(reject_below);
    let job = app.session.plan_cull_job(&ids, reject, pick_best).map_err(|error| error.to_string())?;
    let total = job.photos.len() as u64;
    let cancel = Arc::new(AtomicBool::new(false));
    let guard = app.session.activity.start("cull", "Suggest Culling", Cancel::Flag(cancel.clone()));
    guard.progress(0, total);
    let handle = guard.handle();
    let stop = cancel.clone();
    let rx = background("lightcraft-cull", move || {
        let outcome = job.run(&|fraction, stage| {
            handle.progress((fraction.clamp(0.0, 1.0) * total as f32) as u64, total);
            handle.detail(stage);
            !stop.load(Ordering::Relaxed)
        });
        (job, outcome)
    })?;
    app.cull_review.phase = Phase::Analyzing(Run { rx, _guard: guard, cancel });
    Ok(json!({"background": true, "total": total}))
}

/// Apply: re-measure the accepted photos off the UI thread, then commit their flags as one undo step.
/// Refused (the review stays open) when nothing is accepted or the proposal is stale.
pub fn apply(app: &mut LightcraftApp) -> Result<(), String> {
    let params = match &app.cull_review.phase {
        Phase::Review(review) if review.accepted() > 0 => review.apply_params(),
        Phase::Review(_) => return Err(crate::i18n::tr("No suggestion is accepted").to_string()),
        _ => return Err(crate::i18n::tr("There is no culling proposal to apply").to_string()),
    };
    let prepared = app.session.plan_cull_apply(&params).map_err(|error| error.to_string())?;
    let total = prepared.job.photos.len() as u64;
    let cancel = Arc::new(AtomicBool::new(false));
    let guard = app.session.activity.start("cullApply", "Apply Culling", Cancel::Flag(cancel.clone()));
    guard.progress(0, total);
    let handle = guard.handle();
    let stop = cancel.clone();
    let rx = background("lightcraft-cull-apply", move || {
        let outcome = prepared.job.run(&|fraction, stage| {
            handle.progress((fraction.clamp(0.0, 1.0) * total as f32) as u64, total);
            handle.detail(stage);
            !stop.load(Ordering::Relaxed)
        });
        (prepared, outcome)
    })?;
    app.cull_review.phase = Phase::Applying(Run { rx, _guard: guard, cancel });
    Ok(())
}

/// Stop a running analysis or apply. Its result is discarded when it arrives: nothing is committed.
pub fn cancel(app: &mut LightcraftApp) {
    match &app.cull_review.phase {
        Phase::Analyzing(run) => run.cancel.store(true, Ordering::Relaxed),
        Phase::Applying(run) => run.cancel.store(true, Ordering::Relaxed),
        Phase::Idle | Phase::Review(_) => {}
    }
}

/// Close the review without applying. A running job is cancelled first.
fn discard(app: &mut LightcraftApp) {
    cancel(app);
    app.cull_review.phase = Phase::Idle;
}

/// Takes the result of a finished job, if one arrived. Returns a notice for the user, if any.
pub fn poll_state(app: &mut LightcraftApp) -> Option<String> {
    match std::mem::take(&mut app.cull_review.phase) {
        Phase::Analyzing(run) => match run.rx.try_recv() {
            Ok((job, outcome)) => {
                if run.cancel.load(Ordering::Relaxed) {
                    return None;
                }
                match outcome.and_then(|result| app.session.validate_cull_job_result(&job, &result).map_err(|error| error.to_string())) {
                    Ok(value) => {
                        app.cull_review.phase = Phase::Review(Review::new(&value));
                        None
                    }
                    Err(error) => Some(error),
                }
            }
            Err(TryRecvError::Empty) => {
                app.cull_review.phase = Phase::Analyzing(run);
                None
            }
            Err(TryRecvError::Disconnected) => Some(crate::i18n::tr("Culling stopped unexpectedly").to_string()),
        },
        Phase::Applying(run) => match run.rx.try_recv() {
            Ok((prepared, outcome)) => {
                if run.cancel.load(Ordering::Relaxed) {
                    return None;
                }
                match outcome.and_then(|result| app.session.finish_cull_apply(prepared, result).map_err(|error| error.to_string())) {
                    Ok(value) => Some(crate::i18n::tr_format!("Applied {n} culling flags", n = value["accepted"].as_u64().unwrap_or_default())),
                    Err(error) => Some(error),
                }
            }
            Err(TryRecvError::Empty) => {
                app.cull_review.phase = Phase::Applying(run);
                None
            }
            Err(TryRecvError::Disconnected) => Some(crate::i18n::tr("Culling stopped unexpectedly").to_string()),
        },
        phase @ (Phase::Idle | Phase::Review(_)) => {
            app.cull_review.phase = phase;
            None
        }
    }
}

/// Per frame: take a finished job's result and keep the frame coming while one runs.
pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    if let Some(notice) = poll_state(app) {
        app.toast(ctx, notice);
    }
    if matches!(app.cull_review.phase, Phase::Analyzing(_) | Phase::Applying(_)) {
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

/// The review window, while there is one.
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context) {
    if matches!(app.cull_review.phase, Phase::Idle) {
        return;
    }
    let mut open = true;
    let mut action = Action::None;
    egui::Window::new(crate::i18n::tr("Assisted Culling"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(560.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| action = body(&mut app.cull_review.phase, ui));
    if !open {
        action = Action::Discard;
    }
    match action {
        Action::None => {}
        Action::Cancel => cancel(app),
        Action::Discard => discard(app),
        Action::Apply => {
            if let Err(error) = apply(app) {
                app.toast(ctx, error);
            }
        }
        Action::Page(page) => {
            if let Phase::Review(review) = &mut app.cull_review.phase {
                review.page = page;
            }
        }
    }
}

fn body(phase: &mut Phase, ui: &mut egui::Ui) -> Action {
    let mut action = Action::None;
    let working = matches!(phase, Phase::Analyzing(_));
    match phase {
        Phase::Idle => {}
        Phase::Analyzing(_) | Phase::Applying(_) => {
            ui.horizontal(|ui| {
                ui.spinner();
                let text = if working {
                    crate::i18n::tr("Analyzing the photos. Nothing is flagged until you apply a proposal.")
                } else {
                    crate::i18n::tr("Setting the accepted flags.")
                };
                ui.label(text);
                if ui.button(crate::i18n::tr("Stop")).clicked() {
                    action = Action::Cancel;
                }
            });
        }
        Phase::Review(review) => action = review_body(review, ui),
    }
    action
}

fn review_body(review: &mut Review, ui: &mut egui::Ui) -> Action {
    let mut action = Action::None;
    let pages = review.pages();
    review.page = review.page.min(pages - 1);
    let (suggested, accepted) = (review.suggested(), review.accepted());
    if suggested == 0 {
        ui.label(crate::i18n::tr("No photo is suggested for a flag. Try a higher reject score or pick the sharpest of each burst."));
    } else {
        ui.label(crate::i18n::tr_format!(
            "{suggested} suggestions, {accepted} accepted. Nothing is flagged until you apply.",
            suggested = suggested,
            accepted = accepted
        ));
    }
    ui.separator();
    egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
        for (group, indices) in review.page_sections() {
            ui.add_space(6.0);
            let title = match group {
                Some(group) => crate::i18n::tr_format!("Burst {n}", n = group.saturating_add(1)),
                None => crate::i18n::tr("Single shots").to_string(),
            };
            ui.strong(title);
            for index in indices {
                let row = &mut review.rows[index];
                let reasons: Vec<&str> = row.reasons.iter().map(|code| reason_label(code)).collect();
                ui.horizontal(|ui| {
                    match &row.proposed {
                        Some(flag) => {
                            let verb = if flag == "pick" { crate::i18n::tr("Pick") } else { crate::i18n::tr("Reject") };
                            ui.checkbox(&mut row.accept, format!("{verb}: {}", row.name));
                        }
                        None => {
                            ui.label(row.name.as_str());
                        }
                    }
                    ui.weak(format!("sharpness {:.0} · clipped {:.1}%", row.sharpness, row.clipped * 100.0));
                });
                if !reasons.is_empty() {
                    ui.weak(reasons.join(" · "));
                }
            }
        }
    });
    ui.separator();
    ui.horizontal(|ui| {
        if ui.add_enabled(review.page > 0, egui::Button::new(crate::i18n::tr("Previous page"))).clicked() {
            action = Action::Page(review.page - 1);
        }
        ui.label(crate::i18n::tr_format!("Page {n} of {pages}", n = review.page + 1, pages = pages));
        if ui.add_enabled(review.page + 1 < pages, egui::Button::new(crate::i18n::tr("Next page"))).clicked() {
            action = Action::Page(review.page + 1);
        }
    });
    ui.horizontal(|ui| {
        if ui.add_enabled(accepted > 0, egui::Button::new(crate::i18n::tr_format!("Apply {n}", n = accepted))).clicked() {
            action = Action::Apply;
        }
        if ui.button(crate::i18n::tr("Close without applying")).clicked() {
            action = Action::Discard;
        }
    });
    action
}

/// The engine's reason code, in words. Clipping has no reason code: it is shown as a measured share.
fn reason_label(code: &str) -> &str {
    crate::i18n::tr(match code {
        "belowRejectThreshold" => "Soft focus: below the reject score",
        "atOrAboveRejectThreshold" => "Sharp enough for the reject score",
        "burstMatch" => "Look-alike of its burst",
        "bestOfGroup" => "Sharpest of its burst",
        "tiedForBest" => "Tied for sharpest",
        "missingCaptureTime" => "No capture time: not grouped",
        "keepSuggestion" => "Keep",
        "noActionRequested" => "No action requested",
        "reviewRequired" => "Needs your review",
        _ => "No suggestion",
    })
}
