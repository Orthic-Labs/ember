#![cfg(any(target_os = "macos", target_os = "windows"))]

mod fixture_inputs;

use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rightkit_qa::control::{LaunchSpec, Mode, launch};
use rightkit_qa::harness::Harness;
use rightkit_qa::util::sha256_hex;
use rightkit_qa::workspace;
use serde_json::{Value, json};

static RETAINED_LAYOUT_IDENTITY_NONCE: AtomicU64 = AtomicU64::new(0);

fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required for native qualification"))
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/desktop")
}

fn qa_harness(binary: &Path, evidence: &Path, revision: &str, platform: &str, architecture: &str) -> Harness {
    let managed_root = evidence.parent().expect("QA evidence path must have managed parent").to_path_buf();

    Harness::new_in_managed_root("lightcraft-desktop", PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."), managed_root)
        .expect("RightKit QA harness must initialize")
        .with_evidence_root(evidence)
        .with_identity(json!({
            "sourceRevision": revision,
            "platform": platform,
            "architecture": architecture,
            "webview": if platform == "macos" { "WKWebView" } else { "WebView2" },
            "hidden": true,
        }))
        .with_identity_file("ui", binary)
        .expect("QA binary identity must be hashable")
}

fn scenario_workspace(scenario: &rightkit_qa::harness::Scenario) -> rightkit_qa::workspace::QaWorkspace {
    static WORKSPACES: OnceLock<Mutex<HashMap<PathBuf, rightkit_qa::workspace::QaWorkspace>>> = OnceLock::new();
    let workspaces = WORKSPACES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut workspaces = workspaces.lock().expect("scenario workspace cache must remain usable");
    if let Some(workspace) = workspaces.get(scenario.dir()) {
        return workspace.clone();
    }
    static NEXT_RUN: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_nanos());
    let run_id = format!("lightcraft-{}-{}-{}-{sequence}", scenario.name(), std::process::id(), timestamp);
    let workspace = workspace::create(&scenario.dir().join("control-workspace"), Some(&run_id), "lightcraft")
        .expect("isolated RightKit workspace must initialize");
    workspaces.insert(scenario.dir().to_path_buf(), workspace.clone());
    workspace
}

fn launch_hidden(binary: &Path, scenario: &rightkit_qa::harness::Scenario, _catalog: &Path) -> (rightkit_qa::control::Control, PathBuf) {
    let ws = scenario_workspace(scenario);
    let data = ws.data_dir.clone();
    let env = ws
        .env
        .iter()
        .filter(|(key, _)| {
            // Launch owns this value on every platform; workspace's suite root is separate.
            if key.as_str() == "RIGHTKIT_SUITE_ROOT" {
                return false;
            }
            #[cfg(target_os = "macos")]
            {
                key.starts_with("RIGHTKIT_")
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = key;
                true
            }
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();
    // SDK launch supplies RIGHTKIT_QA_BACKGROUND for both background/hidden modes.
    // App QA handling keeps its actual window invisible without hiding NSApplication.
    #[cfg(target_os = "macos")]
    let launch_binary = binary
        .ancestors()
        .find(|path| path.extension().is_some_and(|extension| extension == "app"))
        .expect("native qualification must launch the SDK-installed app bundle")
        .to_path_buf();
    #[cfg(not(target_os = "macos"))]
    let launch_binary = binary.to_path_buf();
    let spec = LaunchSpec {
        binary: launch_binary,
        mode: if cfg!(target_os = "macos") { Mode::Background } else { Mode::Hidden },
        env,
        startup_timeout: Duration::from_secs(90),
        label: "lightcraft-desktop-native".into(),
    };
    let startup_started = Instant::now();
    let control = launch(&spec, &ws, scenario.tracker()).expect("hidden native app must expose rightkit-control");
    // Control becomes available before React mounts its command subscriptions.
    let ready = catch_unwind(AssertUnwindSafe(|| {
        // Native control may answer in about:blank before WebView navigation.
        // Renderer readiness shares existing launch deadline, rather than receiving
        // an unrelated five-second interaction timeout.
        wait_for_dom_with_timeout(
            &control,
            "return document.querySelector('.lc-content') !== null;",
            spec.startup_timeout.saturating_sub(startup_started.elapsed()),
        );
    }));
    if let Err(payload) = ready {
        let dom = control.eval("return {url: location.href, readyState: document.readyState, title: document.title, body: document.body?.innerText, root: document.getElementById('root')?.innerHTML, width: innerWidth, height: innerHeight};");
        if let Ok(value) = dom
            && let Ok(bytes) = serde_json::to_vec_pretty(&value)
        {
            let _ = fs::write(scenario.dir().join("startup-dom.json"), bytes);
        }
        let _ = control.screenshot_to(&scenario.dir().join("startup-native.png"));
        resume_unwind(payload);
    }
    let focused = control
        .eval("document.activeElement?.blur(); document.body.tabIndex = -1; document.body.focus(); return document.activeElement === document.body;")
        .expect("native shortcut target must focus after renderer readiness");
    assert_eq!(focused.as_bool(), Some(true), "native shortcuts must target app content");
    (control, data)
}

fn run(control: &rightkit_qa::control::Control, id: &str, params: Value) -> Value {
    control.command("lc_run", &json!({"id": id, "params": params})).unwrap_or_else(|error| panic!("{id} failed: {error}"))
}

fn snapshot(control: &rightkit_qa::control::Control) -> Value {
    control.command("lc_snapshot", &Value::Null).expect("native snapshot must reply")
}

fn wait_for_snapshot(control: &rightkit_qa::control::Control, ready: impl Fn(&Value) -> bool, message: &str) -> Value {
    for _ in 0..100 {
        let value = snapshot(control);
        if ready(&value) {
            return value;
        }
        sleep(Duration::from_millis(50));
    }
    panic!("{message}");
}

fn source_hash(path: &Path) -> String {
    sha256_hex(&fs::read(path).unwrap_or_else(|error| panic!("fixture {} must be readable: {error}", path.display())))
}

fn source_fingerprint(path: &Path) -> String {
    if path.is_dir() { tree_fingerprint(path) } else { source_hash(path) }
}

fn assert_sources_unchanged(paths: &[(&str, &Path)], expected: &[(&str, String)]) {
    assert_eq!(paths.len(), expected.len(), "fixture hash inventory must match source inventory");
    for ((label, path), (expected_label, before)) in paths.iter().zip(expected.iter()) {
        assert_eq!(label, expected_label, "fixture hash inventory labels must match");
        assert_eq!(source_fingerprint(path), before.as_str(), "native journey mutated source fixture {label}");
    }
}

fn wait_for_dom(control: &rightkit_qa::control::Control, expression: &str) -> Value {
    wait_for_dom_with_timeout(control, expression, Duration::from_secs(5))
}

fn wait_for_dom_with_timeout(control: &rightkit_qa::control::Control, expression: &str, timeout: Duration) -> Value {
    let started = Instant::now();
    loop {
        let value = control.eval(expression).expect("DOM route query must execute");
        if value.as_bool() == Some(true) {
            return value;
        }
        if started.elapsed() >= timeout {
            panic!("DOM route condition did not become true: {expression}; last result={value}; elapsed={:?}", started.elapsed());
        }
        sleep(Duration::from_millis(50));
    }
}

fn wait_for_retained_layout_collapse(control: &rightkit_qa::control::Control, key: &str, owner: &str) {
    let expression = format!(
        "return (() => {{ const retained = window[{key:?}]; const inspector = document.querySelector('.lc-inspector'); const layout = inspector?.closest('.lc-stage-layout,.lc-library-layout'); const stage = document.querySelector('.stage-workspace'); const currentPreview = document.querySelector('img.stage-preview'); const currentFrame = currentPreview?.closest('.photo-preview-frame'); const collapse = inspector?.querySelector('button[data-inspector-collapse]'); const rail = inspector?.querySelector('.lc-inspector__rail')?.getBoundingClientRect(); const layoutRect = layout?.getBoundingClientRect(); const ready = Boolean(retained && retained.owner === {owner:?} && inspector && layout && stage && currentPreview && currentFrame && collapse && currentPreview.complete && currentPreview.naturalWidth > 0 && currentPreview.naturalHeight > 0 && inspector.classList.contains('is-collapsed') && layout.classList.contains('is-inspector-collapsed') && Math.abs(inspector.getBoundingClientRect().width - 44) < 2 && rail && Math.abs(rail.right - innerWidth) < 2 && retained.layout !== layout && retained.stage === stage && retained.inspector === inspector && retained.collapse === collapse && retained.collapse === document.activeElement && retained.previewFrame === currentFrame); return {{ ready, owned: Boolean(retained && retained.owner === {owner:?}), layoutChanged: Boolean(retained && retained.layout !== layout), stageSame: Boolean(retained && retained.stage === stage), inspectorSame: Boolean(retained && retained.inspector === inspector), collapseSame: Boolean(retained && retained.collapse === collapse), focusSame: Boolean(retained && retained.collapse === document.activeElement), previewFrameSame: Boolean(retained && retained.previewFrame === currentFrame), previewImageChanged: Boolean(retained && retained.preview !== currentPreview), previewReady: Boolean(currentPreview && currentPreview.complete && currentPreview.naturalWidth > 0 && currentPreview.naturalHeight > 0), collapsed: Boolean(inspector?.classList.contains('is-collapsed')), columns: layout ? getComputedStyle(layout).gridTemplateColumns : null, inspectorWidth: inspector?.getBoundingClientRect().width ?? null, layoutWidth: layoutRect?.width ?? null }}; }})();",
        key = key,
        owner = owner,
    );
    let mut last = Value::Null;
    for _ in 0..100 {
        last = control.eval(&expression).expect("retained layout collapse query must execute");
        if last["ready"].as_bool() == Some(true) {
            return;
        }
        sleep(Duration::from_millis(50));
    }
    panic!("retained layout collapse identity did not settle: {last}");
}

fn assert_library_header_contrast(control: &rightkit_qa::control::Control, mode: &str) {
    let colors = control
        .eval(
            "return (() => { const header = document.querySelector('.lc-library-header'); const title = document.querySelector('.lc-library-title h1'); const actions = Array.from(document.querySelectorAll('.lc-library-header .lc-header-action')).slice(0, 2); if (!header || !title || actions.length < 2) return null; const rgb = (value) => { const match = value.match(/rgba?\\(([^)]+)\\)/); if (!match) return null; return match[1].split(/[,\\s/]+/).slice(0, 3).map(Number); }; const luminance = (value) => { const channels = rgb(value); if (!channels || channels.length < 3 || channels.some((channel) => !Number.isFinite(channel))) return null; return channels.map((channel) => { const normalized = channel / 255; return normalized <= 0.03928 ? normalized / 12.92 : ((normalized + 0.055) / 1.055) ** 2.4; }).reduce((sum, channel, index) => sum + channel * [0.2126, 0.7152, 0.0722][index], 0); }; const background = getComputedStyle(header).backgroundColor; const titleColor = getComputedStyle(title).color; const actionColors = actions.map((node) => getComputedStyle(node).color); const bg = luminance(background); const titleLum = luminance(titleColor); const actionLums = actionColors.map(luminance); const ratio = (foreground) => bg === null || foreground === null ? null : (Math.max(bg, foreground) + 0.05) / (Math.min(bg, foreground) + 0.05); return { background, titleColor, actionColors, titleRatio: ratio(titleLum), actionRatios: actionLums.map(ratio) }; })();",
        )
        .expect("library header contrast query must execute");
    assert!(colors.is_object(), "{mode} library header must render colors");
    let title_ratio = colors["titleRatio"].as_f64().unwrap_or(0.0);
    let action_ratios = colors["actionRatios"].as_array().expect("library Import & Export colors must be reported");
    eprintln!("[qa] {mode} library header contrast: {colors}");
    if colors["titleRatio"].as_f64().unwrap_or(0.0) < 4.5 {
        // Tell a stale cascade (a product bug) from hidden WKWebView style invalidation lag:
        // measure again a few frames later before failing on the first reading.
        sleep(Duration::from_millis(250));
        let later = control.eval(
            "return (() => { const workspace = document.querySelector('.lc-library-workspace'); const title = document.querySelector('.lc-library-title h1'); const root = getComputedStyle(document.documentElement); return { theme: document.documentElement.getAttribute('data-theme'), rootInk: root.getPropertyValue('--rk-ink').trim(), rootText: root.getPropertyValue('--lc-text').trim(), workspaceText: workspace ? getComputedStyle(workspace).getPropertyValue('--lc-text').trim() : null, workspaceColor: workspace ? getComputedStyle(workspace).color : null, titleColor: title ? getComputedStyle(title).color : null }; })();",
        );
        eprintln!("[qa] {mode} library header contrast 250 ms later: {later:?}");
    }
    assert!(title_ratio >= 4.5, "{mode} library title contrast must meet WCAG AA: {colors}");
    assert!(
        action_ratios.len() >= 2 && action_ratios.iter().all(|ratio| ratio.as_f64().is_some_and(|value| value >= 4.5)),
        "{mode} Import & Export contrast must meet WCAG AA: {colors}"
    );
}

fn assert_filmstrip_context_contrast(control: &rightkit_qa::control::Control, mode: &str) {
    let colors = control
        .eval(
            r####"return (() => {
                // Hidden WebKit suspends CSS transition clocks. Inspect final chrome colors,
                // recording each settled transition rather than injecting QA-only styles.
                const settledTransitions = [];
                if (document.visibilityState === 'hidden') {
                    for (const animation of document.getAnimations()) {
                        const target = animation.effect?.target;
                        if (animation instanceof CSSTransition && target?.closest('.rk-side, .rk-top')) {
                            settledTransitions.push({ target: target.className, property: animation.transitionProperty, time: animation.currentTime });
                            animation.finish();
                        }
                    }
                }
                const activeSource = document.querySelector('.rk-item.is-active .rk-item__label');
                const sidebarToggle = document.querySelector('.rk-top__toggle');
                const context = document.querySelector('.lc-filmstrip-context');
                const position = document.querySelector('.lc-filmstrip-position');
                const sourceChip = document.querySelector('.lc-filmstrip-context-chip');
                const auto = [...document.querySelectorAll('button.lc-inspector__button.primary')].find((node) => node.textContent?.trim() === 'Auto');
                if (!context || !position || !sourceChip || !auto || !activeSource || !sidebarToggle) return null;
                const canvas = document.createElement('canvas');
                canvas.width = 1;
                canvas.height = 1;
                const painter = canvas.getContext('2d');
                const clamp = (value, min, max) => Math.max(min, Math.min(max, value));
                const parseFunctional = (value) => {
                    const match = value.trim().match(/^rgba?\(([^)]+)\)$/i);
                    if (!match) return null;
                    const parts = match[1].split(/[,\s/]+/).filter(Boolean);
                    if (parts.length < 3) return null;
                    const channel = (part) => { const number = Number.parseFloat(part); return Number.isFinite(number) ? clamp(part.endsWith('%') ? number * 2.55 : number, 0, 255) : null; };
                    const alpha = parts[3] === undefined ? 1 : Number.parseFloat(parts[3]) / (parts[3].endsWith('%') ? 100 : 1);
                    const channels = parts.slice(0, 3).map(channel);
                    return channels.every((value) => value !== null) && Number.isFinite(alpha) ? { r: channels[0], g: channels[1], b: channels[2], a: clamp(alpha, 0, 1) } : null;
                };
                const parseHex = (value) => {
                    const match = value.trim().match(/^#([0-9a-f]+)$/i);
                    if (!match || ![3, 4, 6, 8].includes(match[1].length)) return null;
                    const hex = match[1].length < 5 ? match[1].split('').map((digit) => digit + digit).join('') : match[1];
                    const number = (offset) => Number.parseInt(hex.slice(offset, offset + 2), 16);
                    return { r: number(0), g: number(2), b: number(4), a: hex.length === 8 ? number(6) / 255 : 1 };
                };
                const parseColor = (value) => {
                    const direct = parseFunctional(value) || parseHex(value);
                    if (direct || !painter) return direct;
                    try {
                        painter.fillStyle = 'rgb(1, 2, 3)';
                        const marker = painter.fillStyle;
                        painter.fillStyle = value;
                        if (painter.fillStyle === marker && value.trim() !== marker) return null;
                        painter.clearRect(0, 0, 1, 1);
                        painter.fillRect(0, 0, 1, 1);
                        const pixel = painter.getImageData(0, 0, 1, 1).data;
                        return { r: pixel[0], g: pixel[1], b: pixel[2], a: pixel[3] / 255 };
                    } catch {
                        return null;
                    }
                };
                const blend = (foreground, background) => {
                    const alpha = foreground.a + background.a * (1 - foreground.a);
                    if (alpha <= 0) return { r: 0, g: 0, b: 0, a: 0 };
                    return {
                        r: (foreground.r * foreground.a + background.r * background.a * (1 - foreground.a)) / alpha,
                        g: (foreground.g * foreground.a + background.g * background.a * (1 - foreground.a)) / alpha,
                        b: (foreground.b * foreground.a + background.b * background.a * (1 - foreground.a)) / alpha,
                        a: alpha,
                    };
                };
                const effectiveBackground = (node) => {
                    const ancestors = [];
                    for (let current = node; current; current = current.parentElement) ancestors.unshift(current);
                    let background = { r: 255, g: 255, b: 255, a: 1 };
                    ancestors.forEach((ancestor) => {
                        const color = parseColor(getComputedStyle(ancestor).backgroundColor);
                        if (!color) background = null;
                        else if (background) background = blend(color, background);
                    });
                    return background;
                };
                const luminance = (color) => {
                    const channel = (value) => { const normalized = value / 255; return normalized <= 0.03928 ? normalized / 12.92 : ((normalized + 0.055) / 1.055) ** 2.4; };
                    return channel(color.r) * 0.2126 + channel(color.g) * 0.7152 + channel(color.b) * 0.0722;
                };
                const measure = (node) => {
                    const style = getComputedStyle(node);
                    const foreground = parseColor(style.color);
                    const background = effectiveBackground(node);
                    if (!foreground || !background) return { color: style.color, background, ratio: null };
                    const composited = blend(foreground, background);
                    const foregroundLum = luminance(composited);
                    const backgroundLum = luminance(background);
                    return { color: style.color, background, ratio: (Math.max(foregroundLum, backgroundLum) + 0.05) / (Math.min(foregroundLum, backgroundLum) + 0.05) };
                };
                return { context: measure(context), position: measure(position), sourceChip: measure(sourceChip), auto: measure(auto), activeSource: measure(activeSource), sidebarToggle: measure(sidebarToggle), settledTransitions };
            })();"####,
        )
        .expect("filmstrip contrast query must execute");
    assert!(colors.is_object(), "{mode} filmstrip context must render contrast targets");
    eprintln!("[qa] {mode} filmstrip context contrast: {colors}");
    for (label, key) in [
        ("context", "context"),
        ("position", "position"),
        ("source chip", "sourceChip"),
        ("Auto", "auto"),
        ("active source", "activeSource"),
        ("sidebar toggle", "sidebarToggle"),
    ] {
        let ratio = colors[key]["ratio"].as_f64().unwrap_or(0.0);
        assert!(ratio >= 4.5, "{mode} filmstrip {label} contrast must meet WCAG AA: {colors}");
    }
}

fn click_dom(control: &rightkit_qa::control::Control, selector: &str, message: &str) {
    wait_for_dom(
        control,
        &format!(
            "return (() => {{ const node = document.querySelector({selector:?}); const rect = node?.getBoundingClientRect(); return Boolean(rect && rect.width > 0 && rect.height > 0); }})();"
        ),
    );
    let geometry = control
        .eval(&format!(
            "return (() => {{ const node = document.querySelector({selector:?}); if (!node) return null; const rect = node.getBoundingClientRect(); const x = rect.x + rect.width / 2; const y = rect.y + rect.height / 2; const hit = document.elementFromPoint(x, y); const describe = (value) => value ? {{ tag: value.tagName, aria: value.getAttribute('aria-label'), title: value.getAttribute('title'), className: value.className }} : null; window.__rkClickDiagnostic = []; const capture = (event) => {{ const target = event.target && event.target.closest ? event.target.closest('button,[role=button]') : event.target; if (window.__rkClickDiagnostic.length < 64) window.__rkClickDiagnostic.push({{ type: event.type, isTrusted: event.isTrusted, clientX: event.clientX, clientY: event.clientY, target: describe(target), selected: target?.classList?.contains('selected') === true }}); }}; window.__rkClickDiagnosticCleanup = () => {{ ['pointerdown', 'pointerup', 'click'].forEach((type) => document.removeEventListener(type, capture, true)); }}; ['pointerdown', 'pointerup', 'click'].forEach((type) => document.addEventListener(type, capture, true)); return {{ x, y, rect: {{ x: rect.x, y: rect.y, width: rect.width, height: rect.height }}, target: describe(node), hit: describe(hit), viewport: {{ width: window.innerWidth, height: window.innerHeight, scale: window.devicePixelRatio }} }}; }})();"
        ))
        .expect("DOM click geometry query must execute");
    let x = geometry["x"].as_f64().expect("DOM click x must be numeric");
    let y = geometry["y"].as_f64().expect("DOM click y must be numeric");
    let result = control.click(x, y, "left", 1);
    let events = control
        .eval("return (() => { const events = window.__rkClickDiagnostic || []; if (window.__rkClickDiagnosticCleanup) window.__rkClickDiagnosticCleanup(); delete window.__rkClickDiagnostic; delete window.__rkClickDiagnosticCleanup; return events; })();")
        .unwrap_or(Value::Null);
    eprintln!("[qa] click selector={selector:?} geometry={geometry} events={events}");
    result.expect(message);
}

fn choose_inspector_panel(control: &rightkit_qa::control::Control, panel: &str) {
    let selector = format!(".lc-inspector button[data-panel='{panel}']");
    let visible = control.eval(&format!("return document.querySelector({selector:?}) !== null;")).expect("tool visibility must be queryable");
    if visible.as_bool() != Some(true) {
        click_dom(control, ".lc-inspector button[data-more-tools]", "More tools menu must open");
        wait_for_dom(control, &format!("return document.querySelector({selector:?}) !== null;"));
    }
    click_dom(control, &selector, "inspector tool must select its actual panel");
}

fn assert_inspector_panels_fit(control: &rightkit_qa::control::Control) {
    let before = snapshot(control);
    run(control, "version.create", json!({"name": "A deliberately long saved version name for narrow inspector layout"}));
    for (panel, title) in [
        ("edit", "Edit"),
        ("crop", "Crop & Geometry"),
        ("remove", "Remove"),
        ("masking", "Masking"),
        ("presets", "Presets"),
        ("info", "Info"),
        ("profiles", "Profiles"),
        ("redeye", "Red Eye"),
        ("versions", "Versions"),
        ("activity", "History"),
        ("keywords", "Keywords"),
    ] {
        choose_inspector_panel(control, panel);
        wait_for_dom(
            control,
            &format!(
                "return (() => {{ const e = document.querySelector('.lc-inspector__header'); return !!e && e.textContent.includes({title:?}) && !e.textContent.includes('Loading'); }})();"
            ),
        );
        let bounds = control.eval("return (() => { const e = document.querySelector('.lc-inspector__scroll'); return {width:e.clientWidth, scrollWidth:e.scrollWidth}; })();").expect("inspector overflow must be measurable");
        eprintln!("[qa] inspector {panel}: {bounds}");
        let width = bounds["width"].as_u64().expect("inspector width must be numeric");
        let scroll_width = bounds["scrollWidth"].as_u64().expect("inspector scroll width must be numeric");
        assert!(scroll_width <= width + 1, "inspector {panel} controls must fit without horizontal scrolling: {bounds}");
    }
    run(control, "edit.undo", json!({}));
    assert_eq!(snapshot(control)["undo"], before["undo"], "layout fixture version must be undone");
    choose_inspector_panel(control, "edit");
    wait_for_dom(control, "return document.querySelector('input[aria-label=Exposure]') !== null;");
}

fn assert_workspace_design(control: &rightkit_qa::control::Control) {
    let design = control.eval(r#"return (() => {
        const rect = (selector) => document.querySelector(selector)?.getBoundingClientRect();
        const side = rect('.rk-side'), body = rect('.lc-inspector__content'), rail = rect('.lc-inspector__rail'), film = rect('.lc-filmstrip');
        const tools = [...document.querySelectorAll('.lc-inspector__rail-tools button')].map(node => node.getAttribute('aria-label'));
        return { sidebar: side?.width, inspector: body?.width, rail: rail?.width, filmstrip: film?.height, tools,
            duplicateTools: document.querySelectorAll('.stage-toolstrip,.lc-inspector__tools').length,
            overflow: document.documentElement.scrollWidth > innerWidth,
            footerRows: (() => { const buttons = [...document.querySelectorAll('.stage-footer button')].filter(node => node.getBoundingClientRect().width > 0 && !node.closest('.zoom-menu-popover')); const bounds = buttons.map(node => node.getBoundingClientRect()); return bounds.length ? (Math.max(...bounds.map(r => r.top)) < Math.min(...bounds.map(r => r.bottom)) ? 1 : 2) : 0; })(),
            comparisonLabel: [...document.querySelectorAll('.stage-footer button')].some(node => node.textContent.includes('Before / After')) };
    })();"#).expect("approved workspace geometry must be measurable");
    assert!(design["sidebar"].as_f64().is_some_and(|value| (value - 246.0).abs() < 2.0), "left source sidebar must remain open at246px: {design}");
    assert!(design["inspector"].as_f64().is_some_and(|value| (value - 292.0).abs() < 2.0), "inspector content must measure292px: {design}");
    assert!(design["rail"].as_f64().is_some_and(|value| (value - 44.0).abs() < 2.0), "single tool rail must measure44px: {design}");
    assert!(design["filmstrip"].as_f64().is_some_and(|value| (value - 108.0).abs() < 2.0), "bottom filmstrip must measure108px: {design}");
    assert_eq!(design["tools"], json!(["Edit", "Crop", "Heal", "Mask", "Presets", "Info"]));
    assert_eq!(design["duplicateTools"].as_u64(), Some(0));
    assert_eq!(design["overflow"].as_bool(), Some(false));
    assert_workspace_surfaces_unclipped(control);
    if design["footerRows"].as_u64().unwrap_or(0) > 0 {
        assert_eq!(design["footerRows"].as_u64(), Some(1), "stage footer must stay in one row: {design}");
        assert_eq!(design["comparisonLabel"].as_bool(), Some(true));
    }
}

fn assert_workspace_surfaces_unclipped(control: &rightkit_qa::control::Control) {
    let bounds = control.eval(r#"return (() => {
        const side = document.querySelector('.rk-side')?.getBoundingClientRect();
        const foot = document.querySelector('.rk-side__foot')?.getBoundingClientRect();
        const scroll = document.querySelector('.lc-filmstrip-scroll');
        if (!side || !foot || !scroll) return null;
        const viewport = scroll.getBoundingClientRect();
        const contentBottom = viewport.top + scroll.clientHeight;
        const items = [...scroll.querySelectorAll('.lc-filmstrip-item')].map(e => e.getBoundingClientRect()).filter(r => r.right > viewport.left + 4 && r.left < viewport.right - 4);
        const labels = [...scroll.querySelectorAll('.lc-filmstrip-meta')].filter(e => {
            const r = e.getBoundingClientRect();
            return e.textContent.trim() && r.width > 0 && r.left >= viewport.left && r.right <= viewport.right;
        });
        return { footerGap: side.bottom - foot.bottom, visibleItems: items.length,
            clippedItems: items.filter(r => r.top - 4 < viewport.top - 1 || r.bottom + 4 > contentBottom + 1).length,
            visibleLabels: labels.length,
            coveredLabels: labels.filter(e => { const r = e.getBoundingClientRect(); return !e.contains(document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2)); }).length,
            viewport: { top: viewport.top, contentBottom, bottom: viewport.bottom },
            itemBounds: items.map(r => ({ top: r.top, bottom: r.bottom })) };
    })();"#).expect("sidebar footer & filmstrip clipping must be measurable");
    eprintln!("[qa] unclipped workspace surfaces: {bounds}");
    assert!(bounds["footerGap"].as_f64().is_some_and(|gap| gap.abs() <= 1.0), "brand footer must anchor to sidebar bottom: {bounds}");
    assert!(bounds["visibleItems"].as_u64().is_some_and(|count| count > 0), "filmstrip must show actual thumbnails: {bounds}");
    assert_eq!(bounds["clippedItems"].as_u64(), Some(0), "thumbnail & selection outline must fit above native scrollbar: {bounds}");
    assert_eq!(bounds["coveredLabels"].as_u64(), Some(0), "decoded preview layers must not cover filmstrip filenames or badges: {bounds}");
}

fn assert_sustained_preview_gestures(control: &rightkit_qa::control::Control, evidence: &Path) {
    wait_for_rendered_preview(control, "img.stage-preview", None);
    wait_for_rendered_preview(control, "img.lc-filmstrip-preview", None);
    let baseline = snapshot(control);
    let exposure = baseline["develop"]["light"]["exposure"].clone();
    let undo = baseline["undo"].as_u64().expect("gesture baseline undo count required");
    control.eval(r#"return (() => {
        const t = {samples:0, rafFrames:0, blank:0, unready:0, errors:[], imageErrors:[], blankSamples:[], filmBlank:0, filmUnready:0, phase:"setup", stopped:false};
        const read = () => {
            const images = [...document.querySelectorAll('img.stage-preview')].filter(e => {
                const css = getComputedStyle(e), r = e.getBoundingClientRect();
                return css.opacity !== '0' && css.visibility !== 'hidden' && css.display !== 'none' && r.width > 0 && r.height > 0;
            });
            const errors = [...document.querySelectorAll('.stage-workspace [data-preview-state="error"]')].map(e => e.textContent);
            if (errors.length && t.errors.length < 200) t.errors.push({at:performance.now(), errors});
            return images;
        };
        t.observer = new MutationObserver(read);
        t.observer.observe(document.querySelector('.stage-workspace'), {childList:true, subtree:true, attributes:true, attributeFilter:['src','style']});
        t.onError = e => { if (e.target instanceof HTMLImageElement && t.imageErrors.length < 200) t.imageErrors.push({at:performance.now(),src:e.target.src}); };
        document.querySelector('.stage-workspace').addEventListener('error', t.onError, true);
        const sample = () => {
            if (t.stopped) return;
            const images = read(); t.samples++;
            const filmBounds=document.querySelector('.lc-filmstrip-scroll')?.getBoundingClientRect();
            const filmImages=[...document.querySelectorAll('img.lc-filmstrip-preview')].filter(e=>{const r=e.getBoundingClientRect();return filmBounds&&r.right>filmBounds.left&&r.left<filmBounds.right&&r.width>0&&r.height>0&&getComputedStyle(e).opacity!=='0';});
            if (!filmImages.length) t.filmBlank++;
            else if (!filmImages.some(e=>e.complete&&e.naturalWidth>0)) t.filmUnready++;
            if (!images.length) {
                t.blank++;
                if (t.blankSamples.length < 160) t.blankSamples.push({at:performance.now(),phase:t.phase,
                    images:[...document.querySelectorAll('img.stage-preview')].map(e => {
                        const css=getComputedStyle(e), r=e.getBoundingClientRect(), parent=e.parentElement.getBoundingClientRect();
                        return {src:e.src,opacity:css.opacity,visibility:css.visibility,display:css.display,
                            rect:{width:r.width,height:r.height},parent:{width:parent.width,height:parent.height},
                            complete:e.complete,naturalWidth:e.naturalWidth};
                    }),states:[...document.querySelectorAll('.stage-workspace [data-preview-state]')].map(e=>({state:e.dataset.previewState,text:e.textContent.slice(0,80)}))});
            }
            else if (!images.some(e => e.complete && e.naturalWidth > 0 && e.naturalHeight > 0)) t.unready++;
            t.timer = setTimeout(sample, 50);
        };
        const rafTick = () => {
            if (t.stopped) return;
            t.rafFrames++;
            t.raf = requestAnimationFrame(rafTick);
        };
        t.timer = setTimeout(sample, 0);
        t.raf = requestAnimationFrame(rafTick);
        window.__lcPreviewStress = t; return true;
    })();"#).expect("preview stress observer must initialize");
    let mut rounds = Vec::new();
    let result = catch_unwind(AssertUnwindSafe(|| {
        for index in 0..8 {
            let slider = control
                .eval(
                    r#"return (() => {
                const e = document.querySelector('input[aria-label="Exposure"]');
                e.scrollIntoView({block:'center'}); const r = e.getBoundingClientRect();
                return {x:r.x,y:r.y+r.height/2,width:r.width,min:+e.min,max:+e.max,value:+e.value};
            })();"#,
                )
                .expect("sustained slider geometry must execute");
            let x = slider["x"].as_f64().expect("slider x required");
            let y = slider["y"].as_f64().expect("slider y required");
            let width = slider["width"].as_f64().expect("slider width required");
            let min = slider["min"].as_f64().expect("slider min required");
            let max = slider["max"].as_f64().expect("slider max required");
            let value = slider["value"].as_f64().expect("slider value required");
            assert!(max > min, "slider range must be nonempty");
            let start = ((value - min) / (max - min)).clamp(0.0, 1.0);
            let preferred = if index % 2 == 0 { 0.7_f64 } else { 0.3_f64 };
            let target = if (preferred - start).abs() < 0.1 { 1.0 - preferred } else { preferred };
            let steps = if index < 4 { 48 } else { 96 };
            let prior = wait_for_rendered_preview(control, "img.stage-preview", None);
            control.eval(&format!("return window.__lcPreviewStress.phase='drag {index}';")).expect("drag phase must be recorded");
            control.drag((x + width * start, y), (x + width * target, y), steps).expect("sustained pointer drag must execute");
            let edited = wait_for_snapshot(
                control,
                |v| v["undo"].as_u64() == Some(undo + 1) && v["develop"]["light"]["exposure"] != exposure,
                "sustained pointer drag must settle as one undo step",
            );
            let rendered = wait_for_rendered_preview(control, "img.stage-preview", prior["src"].as_str());
            rounds.push(
                json!({"index":index,"steps":steps,"undo":edited["undo"],"exposure":edited["develop"]["light"]["exposure"],"decoded":rendered}),
            );
            control.eval(&format!("return window.__lcPreviewStress.phase='undo {index}';")).expect("undo phase must be recorded");
            run(control, "edit.undo", json!({}));
            wait_for_snapshot(
                control,
                |v| v["undo"].as_u64() == Some(undo) && v["develop"]["light"]["exposure"] == exposure,
                "stress undo must restore baseline",
            );
            wait_for_rendered_preview(control, "img.stage-preview", rendered["src"].as_str());
            sleep(Duration::from_millis(300));
        }
    }));
    let trace = control
        .eval(
            r#"return (() => {
        const t = window.__lcPreviewStress; if (!t) return null;
        t.stopped = true; t.observer.disconnect(); clearTimeout(t.timer); cancelAnimationFrame(t.raf);
        document.querySelector('.stage-workspace').removeEventListener('error',t.onError,true);
        delete window.__lcPreviewStress;
        return {samples:t.samples,rafFrames:t.rafFrames,blank:t.blank,unready:t.unready,errors:t.errors,imageErrors:t.imageErrors,blankSamples:t.blankSamples,filmBlank:t.filmBlank,filmUnready:t.filmUnready};
    })();"#,
        )
        .expect("preview stress observer cleanup must execute");
    let receipt = json!({"baseline":baseline,"rounds":rounds,"trace":trace,"restored":snapshot(control)["develop"]["light"]["exposure"] == exposure});
    fs::write(evidence, serde_json::to_vec_pretty(&receipt).expect("stress receipt must serialize")).expect("stress receipt must be saved");
    if let Err(payload) = result {
        resume_unwind(payload);
    }
    assert_eq!(rounds.len(), 8, "all fast & slow sustained gestures must execute");
    assert!(trace["samples"].as_u64().is_some_and(|count| count >= 10), "timer-driven preview samples must run: {trace}");
    assert_eq!(trace["blank"].as_u64(), Some(0), "mounted pixels must never disappear during same-photo editing: {trace}");
    assert_eq!(trace["unready"].as_u64(), Some(0), "presented pixels must stay decoded during handoff: {trace}");
    assert_eq!(trace["filmBlank"].as_u64(), Some(0), "filmstrip pixels must persist during same-photo editing: {trace}");
    assert_eq!(trace["filmUnready"].as_u64(), Some(0), "filmstrip pixels must remain decoded during editing: {trace}");
    assert_eq!(trace["errors"].as_array().map(Vec::len), Some(0), "no transient Retry/error banners during valid drag: {trace}");
    assert_eq!(trace["imageErrors"].as_array().map(Vec::len), Some(0), "no transport decode errors during valid drag: {trace}");
    assert_eq!(receipt["restored"].as_bool(), Some(true), "stress must restore baseline edits");
}

fn assert_primary_editing_usable(control: &rightkit_qa::control::Control) {
    let editing = control.eval(r#"return (() => {
        const headings = [...document.querySelectorAll('.lc-inspector__section-head > span:nth-child(2)')].map(node => node.textContent.trim());
        const exposure = document.querySelector('input[id="ctl-light.exposure"]');
        const rect = exposure?.getBoundingClientRect();
        const body = document.querySelector('.lc-inspector__scroll')?.getBoundingClientRect();
        return { headings, visible: !!rect && !!body && rect.width > 200 && rect.top >= body.top && rect.bottom <= body.bottom && rect.bottom <= innerHeight };
    })();"#).expect("primary editing geometry must be queryable");
    let headings = editing["headings"].as_array().expect("editing headings must be present");
    assert!(
        headings.len() >= 2 && headings[0].as_str() == Some("Light") && headings[1].as_str() == Some("Color"),
        "primary Light & Color groups must lead inspector: {editing}"
    );
    assert_eq!(editing["visible"].as_bool(), Some(true), "Exposure must be usable without scrolling inspector: {editing}");
}

fn set_native_viewport(control: &rightkit_qa::control::Control, width: u64, height: u64) -> Value {
    let observed = control
        .set_viewport(u32::try_from(width).expect("viewport width must fit u32"), u32::try_from(height).expect("viewport height must fit u32"))
        .expect("RightKit native viewport resize must execute");
    assert_eq!(u64::from(observed.0), width, "RightKit must report exact CSS viewport width");
    assert_eq!(u64::from(observed.1), height, "RightKit must report exact CSS viewport height");
    let result = json!({"requested": {"width": width, "height": height}, "observedInnerSize": {"width": observed.0, "height": observed.1}, "provider": "rightkit-control"});
    let dom_size = control
        .eval("return {width: window.innerWidth, height: window.innerHeight, scale: window.devicePixelRatio};")
        .expect("WebView viewport diagnostics must execute");
    eprintln!("[qa] viewport native={result}; DOM={dom_size}");
    let settled = wait_for_dom(control, &format!("return window.innerWidth === {width} && window.innerHeight === {height};"));
    assert_eq!(settled.as_bool(), Some(true), "WebView inner size must match requested QA viewport");
    let layout = r#"return (() => {
            const viewport = { width: window.innerWidth, height: window.innerHeight };
            const rect = (selector) => { const node = document.querySelector(selector); if (!node) return null; const r = node.getBoundingClientRect(); return { x:r.x, y:r.y, width:r.width, height:r.height, bottom:r.bottom, display:getComputedStyle(node).display }; };
            const outer = ['html', 'body', '#root', '.rk-shell'].map(rect);
            const body = rect('.rk-body');
            const plane = rect('.rk-plane');
            const content = rect('.lc-content');
            const exact = (r) => r && r.display !== 'none' && Math.abs(r.width - viewport.width) <= 1 && Math.abs(r.height - viewport.height) <= 1 && Math.abs(r.bottom - viewport.height) <= 1;
            const child = (r) => r && r.display !== 'none' && r.width >= 1 && r.height >= 1 && Math.abs(r.bottom - viewport.height) <= 1;
            // on a miss, the rects become `last result` in the timeout message
            return (outer.every(exact) && child(body) && child(plane) && child(content)) || { viewport, outer, body, plane, content };
        })();"#;
    let layout_settled = wait_for_dom(control, layout);
    assert_eq!(layout_settled.as_bool(), Some(true), "native viewport layout must settle after resize");
    assert_native_window_hidden(control);
    result
}

fn diagnose_native_viewport_bounce(control: &rightkit_qa::control::Control, scenario: &rightkit_qa::harness::Scenario, width: u32, height: u32) {
    let sample = |phase: &str| {
        let dom = match control.eval(r#"return (() => {
            const describe = (selector) => {
                const node = document.querySelector(selector);
                if (!node) return { selector, missing: true };
                const rect = node.getBoundingClientRect();
                const style = getComputedStyle(node);
                return {
                    selector,
                    className: typeof node.className === 'string' ? node.className : '',
                    rect: { x: rect.x, y: rect.y, width: rect.width, height: rect.height, right: rect.right, bottom: rect.bottom },
                    inlineStyle: node.getAttribute('style'),
                    top: style.top,
                    columns: style.gridTemplateColumns,
                    display: style.display,
                    scrollTop: node.scrollTop,
                    scrollHeight: node.scrollHeight,
                    clientHeight: node.clientHeight,
                };
            };
            return {
                viewport: { width: innerWidth, height: innerHeight },
                layout: ['.lc-library-layout', '.lc-library-workspace', '.lc-grid-scroll', '.lc-grid-spacer', '.lc-grid-window', '.lc-stage-layout', '.stage-workspace', '.lc-inspector', '.lc-inspector__rail', '.lc-inspector__more-menu'].map(describe),
            };
        })();"#) {
            Ok(value) => value,
            Err(error) => json!({"error": error.to_string()}),
        };
        let native = match control.command("lc_qa_window_state", &Value::Null) {
            Ok(value) => value,
            Err(error) => json!({"error": error.to_string()}),
        };
        json!({"phase": phase, "dom": dom, "native": native})
    };

    let before = sample("before");
    let bounced_to = width
        .checked_add(1)
        .map(|bounce_width| match control.set_viewport(bounce_width, height) {
            Ok((inner_width, inner_height)) => json!({
                "requested": {"width": bounce_width, "height": height},
                "observed": {"width": inner_width, "height": inner_height},
            }),
            Err(error) => json!({
                "requested": {"width": bounce_width, "height": height},
                "error": error.to_string(),
            }),
        })
        .unwrap_or_else(|| json!({"error": "viewport width overflow"}));
    let bounced = sample("bounce-immediate");
    sleep(Duration::from_millis(128));
    let bounced_after_128ms = sample("bounce-after-128ms");
    let restored_to = match control.set_viewport(width, height) {
        Ok((inner_width, inner_height)) => json!({"observed": {"width": inner_width, "height": inner_height}}),
        Err(error) => json!({"error": error.to_string()}),
    };
    let restored = sample("restored-immediate");
    sleep(Duration::from_millis(128));
    let restored_after_128ms = sample("restored-after-128ms");
    let evidence = json!({
        "schema": 1,
        "probe": "native-viewport-bounce",
        "requested": {"width": width, "height": height},
        "before": before,
        "bounce": {"resize": bounced_to, "immediate": bounced, "after128ms": bounced_after_128ms},
        "restored": {"resize": restored_to, "immediate": restored, "after128ms": restored_after_128ms},
    });
    eprintln!("[qa] native viewport bounce diagnostics={evidence}");
    let path = scenario.dir().join("native-viewport-bounce.json");
    if let Ok(bytes) = serde_json::to_vec_pretty(&evidence)
        && fs::write(&path, bytes).is_ok()
    {
        scenario.keep("native-viewport-bounce.json", &path);
    }
}

fn diagnose_stage_child_invalidation(control: &rightkit_qa::control::Control, scenario: &rightkit_qa::harness::Scenario) -> Option<bool> {
    const MAX_EVIDENCE_BYTES: usize = 64 * 1024;
    static PROBE_NONCE: AtomicU64 = AtomicU64::new(0);
    if scenario.name() != "ipc" {
        return None;
    }
    let nonce = PROBE_NONCE.fetch_add(1, Ordering::Relaxed);
    let key = format!("__lcStageChildInvalidationProbe_{nonce}");
    let owner = format!("stage-child-invalidation:{}:{nonce}", scenario.name());
    let setup = format!(
        r#"return (() => {{
            const key = {key:?};
            const owner = {owner:?};
            if (Object.prototype.hasOwnProperty.call(window, key)) return {{ owned: false, owner, error: 'probe state collision' }};
            const selector = '.lc-stage-layout.is-inspector-collapsed';
            const state = {{ owner, selector, target: null, probe: null, replacement: null, stage: null, inspector: null, samples: [], setupError: null, restoreErrors: [], live: true, timer: null }};
            const safeError = (error) => String(error && error.stack ? error.stack : error).slice(0, 512);
            const geometry = (node) => {{
                if (!node) return null;
                const rect = node.getBoundingClientRect();
                const style = getComputedStyle(node);
                return {{
                    rect: {{ x: rect.x, y: rect.y, width: rect.width, height: rect.height, right: rect.right, bottom: rect.bottom }},
                    inline: String(node.getAttribute('style') || '').slice(0, 512),
                    computed: {{ columns: style.gridTemplateColumns, rows: style.gridTemplateRows, width: style.width, height: style.height, display: style.display, position: style.position, visibility: style.visibility }},
                }};
            }};
            const nodeInfo = (node, parent, identity) => {{
                if (!node) return {{ present: false }};
                const layout = node.closest('.lc-stage-layout,.lc-library-layout');
                return {{ present: true, connected: Boolean(node.isConnected), sameParent: Boolean(parent && layout === parent), identity, className: String(node.className || '').slice(0, 256), geometry: geometry(node) }};
            }};
            const probeInfo = (node, parent, identity) => {{
                if (!node) return {{ present: false }};
                const style = getComputedStyle(node);
                return {{ present: true, connected: Boolean(node.isConnected), sameParent: Boolean(parent && node.parentElement === parent), identity, ariaHidden: node.getAttribute('aria-hidden'), position: style.position, width: style.width, height: style.height }};
            }};
            state.sample = (phase) => {{
                const target = state.target;
                state.samples.push({{
                    phase,
                    viewport: {{ width: innerWidth, height: innerHeight, hidden: document.hidden, visibility: document.visibilityState }},
                    target: target ? {{ selector: target.selector, geometry: geometry(target.original), parent: geometry(target.parent) }} : null,
                    stage: nodeInfo(state.stage, target && target.original, 'stage'),
                    inspector: nodeInfo(state.inspector, target && target.original, 'inspector'),
                    probe: probeInfo(state.probe, target && target.original, 'inserted'),
                    replacement: probeInfo(state.replacement, target && target.original, 'replacement'),
                }});
            }};
            state.restore = () => {{
                const target = state.target;
                if (!target || !target.original || !target.original.isConnected) return;
                try {{
                    if (state.replacement && state.replacement.isConnected && state.replacement.parentElement === target.original) target.original.removeChild(state.replacement);
                    if (state.probe && state.probe.isConnected && state.probe.parentElement === target.original) target.original.removeChild(state.probe);
                }} catch (error) {{
                    state.restoreErrors.push({{ error: safeError(error) }});
                }}
            }};
            window[key] = state;
            try {{
                const original = document.querySelector(selector);
                state.target = {{ selector, original, parent: original ? original.parentElement : null }};
                state.stage = original?.querySelector('.stage-workspace') || null;
                state.inspector = original?.querySelector('.lc-inspector') || null;
                state.sample('before');
                if (!original || !state.target.parent || !original.isConnected || original.parentElement !== state.target.parent) throw new Error('original node is disconnected or parent changed');
                const probe = document.createElement('span');
                probe.setAttribute('aria-hidden', 'true');
                probe.setAttribute('data-lc-layout-probe', owner);
                probe.style.cssText = 'position:absolute;inset:0 auto auto 0;width:0;height:0;min-width:0;min-height:0;padding:0;margin:0;border:0;pointer-events:none;visibility:hidden;';
                original.appendChild(probe);
                state.probe = probe;
                state.sample('after-child-insert');
                const replacement = probe.cloneNode(false);
                original.replaceChild(replacement, probe);
                state.replacement = replacement;
                state.sample('after-child-replacement-immediate');
                state.timer = setTimeout(() => {{ if (state.live) state.sample('after-child-replacement-64ms'); }}, 64);
            }} catch (error) {{
                state.setupError = safeError(error);
            }}
            return {{ owned: true, owner: state.owner, selector, setupError: state.setupError, samples: state.samples }};
        }})();"#,
        key = key,
        owner = owner,
    );
    let setup_result = match control.eval(&setup) {
        Ok(value) => value,
        Err(error) => json!({"error": error.to_string().chars().take(512).collect::<String>()}),
    };
    sleep(Duration::from_millis(80));
    let after_64ms = match control.eval(&format!(
        "return (() => {{ const key = {key:?}; const owner = {owner:?}; const state = window[key]; if (!state) return {{owned: false, owner, error: 'probe state missing'}}; if (state.owner !== owner) return {{owned: false, owner, error: 'probe state ownership mismatch'}}; return {{owned: true, owner: state.owner, setupError: state.setupError, samples: state.samples, restoreErrors: state.restoreErrors}}; }})();",
        key = key,
        owner = owner,
    )) {
        Ok(value) => value,
        Err(error) => json!({"error": error.to_string().chars().take(512).collect::<String>()}),
    };
    let cleanup = match control.eval(&format!(
        "return (() => {{ const key = {key:?}; const owner = {owner:?}; const state = window[key]; if (!state) return {{owned: false, restored: false, owner, error: 'probe state missing'}}; if (state.owner !== owner) return {{owned: false, restored: false, owner, error: 'probe state ownership mismatch'}}; let error = null; try {{ if (state.timer) clearTimeout(state.timer); }} catch (timerError) {{ error = String(timerError && timerError.stack ? timerError.stack : timerError).slice(0, 512); }} finally {{ state.live = false; try {{ state.restore(); }} catch (restoreError) {{ error = String(restoreError && restoreError.stack ? restoreError.stack : restoreError).slice(0, 512); }} }} const target = state.target; const before = state.samples.find((sample) => sample.phase === 'before')?.target?.geometry; const after = target && target.original ? (() => {{ const rect = target.original.getBoundingClientRect(); const style = getComputedStyle(target.original); return {{ rect: {{ x: rect.x, y: rect.y, width: rect.width, height: rect.height }}, computed: {{ columns: style.gridTemplateColumns, rows: style.gridTemplateRows }} }}; }})() : null; const parentChanged = Boolean(before && after && (before.computed.columns !== after.computed.columns || before.computed.rows !== after.computed.rows || Math.abs(before.rect.x - after.rect.x) > 0.5 || Math.abs(before.rect.y - after.rect.y) > 0.5 || Math.abs(before.rect.width - after.rect.width) > 0.5 || Math.abs(before.rect.height - after.rect.height) > 0.5)); const restoredTarget = {{connected: Boolean(target && target.original && target.original.isConnected), sameParent: Boolean(target && target.original && target.parent && target.original.parentElement === target.parent), stagePreserved: Boolean(state.stage && state.stage.isConnected && state.stage.closest('.lc-stage-layout,.lc-library-layout') === target.original), inspectorPreserved: Boolean(state.inspector && state.inspector.isConnected && state.inspector.closest('.lc-stage-layout,.lc-library-layout') === target.original)}}; const result = {{owned: true, owner, parentChanged, restored: restoredTarget.connected && restoredTarget.sameParent && restoredTarget.stagePreserved && restoredTarget.inspectorPreserved, restoredTarget, restoreErrors: state.restoreErrors, error}}; if (window[key] === state) delete window[key]; return result; }})();",
        key = key,
        owner = owner,
    )) {
        Ok(value) => value,
        Err(error) => json!({"error": error.to_string().chars().take(512).collect::<String>()}),
    };
    let parent_changed = cleanup["parentChanged"].as_bool();
    let evidence = json!({"schema": 1, "probe": "stage-child-invalidation", "journey": scenario.name(), "setup": setup_result, "after64ms": after_64ms, "cleanup": cleanup});
    let encoded = match serde_json::to_vec_pretty(&evidence) {
        Ok(bytes) if bytes.len() <= MAX_EVIDENCE_BYTES => bytes,
        Ok(bytes) => serde_json::to_vec(&json!({"schema": 1, "probe": "stage-child-invalidation", "journey": scenario.name(), "truncated": true, "encodedBytes": bytes.len(), "maxBytes": MAX_EVIDENCE_BYTES})).unwrap_or_default(),
        Err(error) => serde_json::to_vec(&json!({"schema": 1, "probe": "stage-child-invalidation", "journey": scenario.name(), "error": error.to_string().chars().take(512).collect::<String>()})).unwrap_or_default(),
    };
    eprintln!("[qa] stage child invalidation diagnostics={}", String::from_utf8_lossy(&encoded));
    let path = scenario.dir().join("stage-child-invalidation-layout.json");
    if fs::write(&path, &encoded).is_ok() {
        scenario.keep("stage-child-invalidation-layout.json", &path);
    }
    parent_changed
}

// Failure-only macOS probe for stale layout state on an existing DOM node. Each
// selector gets an isolated replacement in its original parent & sibling slot;
// no offscreen clone is used as a viewport comparison.
fn diagnose_same_parent_layout(
    control: &rightkit_qa::control::Control,
    scenario: &rightkit_qa::harness::Scenario,
    prior_probe_changed_parent: Option<bool>,
) {
    const MAX_EVIDENCE_BYTES: usize = 64 * 1024;
    static PROBE_NONCE: AtomicU64 = AtomicU64::new(0);
    let selectors = match scenario.name() {
        "ipc" => vec![".lc-stage-layout.is-inspector-collapsed"],
        "scalability" => vec![".lc-grid-spacer", ".lc-grid-window"],
        _ => return,
    };
    let mut trials = Vec::new();
    for (index, selector) in selectors.iter().enumerate() {
        let key = format!("__lcSameParentLayoutProbe_{index}");
        let owner = format!("same-parent:{}:{index}:{}", scenario.name(), PROBE_NONCE.fetch_add(1, Ordering::Relaxed));
        let setup = format!(
            r#"return (() => {{
                const key = {key:?};
                const owner = {owner:?};
                if (Object.prototype.hasOwnProperty.call(window, key)) return {{ owned: false, owner, error: 'probe state collision' }};
                const selector = {selector:?};
                const state = {{ owner, selector, target: null, samples: [], setupError: null, restoreErrors: [], live: true, timer: null }};
                const safeError = (error) => String(error && error.stack ? error.stack : error).slice(0, 512);
                const nodeInfo = (node, target) => {{
                    if (!node) return {{ present: false }};
                    const connected = Boolean(node.isConnected);
                    const parent = target.parent;
                    const sameParent = Boolean(parent && node.parentElement === parent);
                    const siblingStable = Boolean(!target.nextSibling || node.nextSibling === target.nextSibling);
                    if (!connected) return {{ present: true, connected: false, sameParent, siblingStable }};
                    const rect = node.getBoundingClientRect();
                    const style = getComputedStyle(node);
                    const parentRect = parent && parent.isConnected ? parent.getBoundingClientRect() : null;
                    return {{
                        present: true,
                        connected: true,
                        sameParent,
                        siblingStable,
                        identity: node === target.original ? 'original' : node === target.replacement ? 'replacement' : 'other',
                        parent: parent ? {{ tag: parent.tagName, id: String(parent.id || '').slice(0, 256), className: String(parent.className || '').slice(0, 256), connected: parent.isConnected }} : null,
                        parentRect: parentRect ? {{ x: parentRect.x, y: parentRect.y, width: parentRect.width, height: parentRect.height, right: parentRect.right, bottom: parentRect.bottom }} : null,
                        nextSibling: target.nextSibling ? {{ tag: target.nextSibling.tagName, id: String(target.nextSibling.id || '').slice(0, 256), className: String(target.nextSibling.className || '').slice(0, 256) }} : null,
                        rect: {{ x: rect.x, y: rect.y, width: rect.width, height: rect.height, right: rect.right, bottom: rect.bottom }},
                        inline: String(node.getAttribute('style') || '').slice(0, 512),
                        computed: {{ top: style.top, left: style.left, width: style.width, height: style.height, columns: style.gridTemplateColumns, rows: style.gridTemplateRows, display: style.display, position: style.position, visibility: style.visibility }},
                        layout: {{ scrollTop: node.scrollTop, scrollHeight: node.scrollHeight, clientHeight: node.clientHeight, scrollWidth: node.scrollWidth, clientWidth: node.clientWidth }},
                    }};
                }};
                state.sample = (phase) => state.samples.push({{
                    phase,
                    viewport: {{ width: innerWidth, height: innerHeight }},
                    target: state.target ? {{
                        selector: state.target.selector,
                        error: state.target.error || null,
                        original: nodeInfo(state.target.original, state.target),
                        replacement: nodeInfo(state.target.replacement, state.target),
                    }} : null,
                }});
                state.restore = () => {{
                    const target = state.target;
                    if (!target) return;
                    try {{
                        if (target.replacement && target.replacement.isConnected && target.replacement.parentElement === target.parent && target.original) {{
                            target.parent.replaceChild(target.original, target.replacement);
                        }} else if (target.original && !target.original.isConnected && target.parent && target.parent.isConnected) {{
                            const sibling = target.nextSibling && target.nextSibling.parentElement === target.parent ? target.nextSibling : null;
                            target.parent.insertBefore(target.original, sibling);
                        }}
                    }} catch (error) {{
                        state.restoreErrors.push({{ selector: target.selector, error: safeError(error) }});
                    }}
                }};
                window[key] = state;
                try {{
                    const original = document.querySelector(selector);
                    state.target = {{ selector, original, replacement: null, parent: original ? original.parentElement : null, nextSibling: original ? original.nextSibling : null, error: original ? null : 'missing original node' }};
                    state.sample('before');
                    const target = state.target;
                    if (!target.original || !target.parent || !target.original.isConnected || target.original.parentElement !== target.parent) {{
                        target.error = target.error || 'original node is disconnected or parent changed';
                    }} else {{
                        target.replacement = target.original.cloneNode(true);
                        target.parent.replaceChild(target.replacement, target.original);
                        state.sample('after-replacement-immediate');
                        state.timer = setTimeout(() => {{ if (state.live) state.sample('after-replacement-64ms'); }}, 64);
                    }}
                }} catch (error) {{
                    state.setupError = safeError(error);
                }}
                return {{ owned: true, owner: state.owner, selector, setupError: state.setupError, samples: state.samples }};
            }})();"#,
            key = key,
            owner = owner,
            selector = selector,
        );
        let setup_result = match control.eval(&setup) {
            Ok(value) => value,
            Err(error) => json!({"error": error.to_string().chars().take(512).collect::<String>()}),
        };
        sleep(Duration::from_millis(80));
        let after_64ms = match control.eval(&format!(
            "return (() => {{ const key = {key:?}; const owner = {owner:?}; const state = window[key]; if (!state) return {{owned: false, owner, error: 'probe state missing'}}; if (state.owner !== owner) return {{owned: false, owner, error: 'probe state ownership mismatch'}}; return {{owned: true, owner: state.owner, setupError: state.setupError, samples: state.samples, restoreErrors: state.restoreErrors}}; }})();",
            key = key,
            owner = owner,
        )) {
            Ok(value) => value,
            Err(error) => json!({"error": error.to_string().chars().take(512).collect::<String>()}),
        };
        let cleanup = match control.eval(&format!(
            "return (() => {{ const key = {key:?}; const owner = {owner:?}; const state = window[key]; if (!state) return {{owned: false, restored: false, owner, error: 'probe state missing'}}; if (state.owner !== owner) return {{owned: false, restored: false, owner, error: 'probe state ownership mismatch'}}; let error = null; try {{ if (state.timer) clearTimeout(state.timer); }} catch (timerError) {{ error = String(timerError && timerError.stack ? timerError.stack : timerError).slice(0, 512); }} finally {{ state.live = false; try {{ state.restore(); }} catch (restoreError) {{ error = String(restoreError && restoreError.stack ? restoreError.stack : restoreError).slice(0, 512); }} }} const target = state.target; const restoredTarget = {{selector: state.selector, connected: Boolean(target && target.original && target.original.isConnected), sameParent: Boolean(target && target.original && target.original.parentElement === target.parent), siblingPreserved: Boolean(target && target.original && target.original.nextSibling === target.nextSibling)}}; const result = {{owned: true, owner, restored: restoredTarget.connected && restoredTarget.sameParent && restoredTarget.siblingPreserved, restoredTarget, restoreErrors: state.restoreErrors, error}}; if (window[key] === state) delete window[key]; return result; }})();",
            key = key,
            owner = owner,
        )) {
            Ok(value) => value,
            Err(error) => json!({"error": error.to_string().chars().take(512).collect::<String>()}),
        };
        trials.push(json!({"selector": selector, "setup": setup_result, "after64ms": after_64ms, "cleanup": cleanup}));
    }
    let evidence = json!({"schema": 1, "probe": "same-parent-dom-replacement", "journey": scenario.name(), "priorProbeChangedParent": prior_probe_changed_parent, "trials": trials});
    let pretty = match serde_json::to_vec_pretty(&evidence) {
        Ok(bytes) => bytes,
        Err(error) => serde_json::to_vec(
            &json!({"schema": 1, "probe": "same-parent-dom-replacement", "error": error.to_string().chars().take(512).collect::<String>()}),
        )
        .unwrap_or_default(),
    };
    let output = if pretty.len() <= MAX_EVIDENCE_BYTES {
        pretty
    } else {
        serde_json::to_vec(&json!({"schema": 1, "probe": "same-parent-dom-replacement", "truncated": true, "encodedBytes": pretty.len(), "maxBytes": MAX_EVIDENCE_BYTES})).unwrap_or_default()
    };
    let output = output.into_iter().take(MAX_EVIDENCE_BYTES).collect::<Vec<_>>();
    eprintln!("[qa] same-parent layout diagnostics={}", String::from_utf8_lossy(&output));
    let path = scenario.dir().join("same-parent-layout.json");
    if fs::write(&path, &output).is_ok() {
        scenario.keep("same-parent-layout.json", &path);
    }
}

fn assert_layout_settled(control: &rightkit_qa::control::Control, selector: &str) {
    let expression = format!(
        "return (() => {{ const node = document.querySelector({selector:?}); if (!node) return false; const rect = node.getBoundingClientRect(); return rect.width >= 1 && rect.height >= 1 && getComputedStyle(node).display !== 'none'; }})();"
    );
    assert_eq!(wait_for_dom(control, &expression).as_bool(), Some(true), "layout must settle for {selector}");
}

fn is_scoped_preview_src(src: &str) -> bool {
    let prefix = if cfg!(target_os = "windows") { "http://lightcraft-preview.localhost/" } else { "lightcraft-preview://localhost/" };
    src.strip_prefix(prefix).is_some_and(|handle| !handle.is_empty() && !handle.contains("..") && !handle.contains('/'))
}

fn wait_for_rendered_preview(control: &rightkit_qa::control::Control, selector: &str, expected_src: Option<&str>) -> Value {
    let mut last = Value::Null;
    let expression = format!(
        "return (() => {{ const img = document.querySelector({selector:?}); if (!img) return {{ready:false, reason:'missing'}}; const cell = img.closest('.lc-photo-cell'); const src = img.getAttribute('src') || ''; return {{ready: img.complete && img.naturalWidth > 0 && img.naturalHeight > 0, naturalWidth: img.naturalWidth, naturalHeight: img.naturalHeight, src, active: Boolean(cell?.classList.contains('is-active')), selected: Boolean(cell?.getAttribute('aria-selected') === 'true')}}; }})();"
    );
    for _ in 0..160 {
        let value = control.eval(&expression).expect("rendered preview DOM query must execute");
        let ready = value["ready"].as_bool() == Some(true);
        let scoped = value["src"].as_str().is_some_and(is_scoped_preview_src);
        let changed = expected_src.is_none_or(|previous| value["src"].as_str() != Some(previous));
        if ready && scoped && changed {
            assert!(value["naturalWidth"].as_u64().is_some_and(|width| width > 0));
            assert!(value["naturalHeight"].as_u64().is_some_and(|height| height > 0));
            if selector.contains("lc-photo-cell") {
                assert_eq!(value["active"].as_bool(), Some(true), "active grid cell must own rendered preview: {value}");
                assert_eq!(value["selected"].as_bool(), Some(true), "active grid cell must be selected: {value}");
            }
            return value;
        }
        last = value;
        sleep(Duration::from_millis(50));
    }
    panic!("rendered preview did not become ready for selector {selector}: {expression}; last result={last}");
}

// Tool fixture has varied RGB pixels. A decoded DOM image can still be absent
// from native compositor output; inspect actual PNG region before accepting it.
fn capture_visible_tool_preview(control: &rightkit_qa::control::Control, path: &Path) {
    let mut last = Value::Null;
    for _ in 0..40 {
        let geometry = control.eval("return (() => { const img = document.querySelector('img.stage-preview'); if (!img) return null; const r = img.getBoundingClientRect(); const css = getComputedStyle(img); const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2); return {src: img.src, complete: img.complete, naturalWidth: img.naturalWidth, naturalHeight: img.naturalHeight, rect: {x:r.x,y:r.y,width:r.width,height:r.height}, viewport:{width:innerWidth,height:innerHeight}, display:css.display, visibility:css.visibility, opacity:css.opacity, hit:hit?.tagName, hitClass:hit?.getAttribute('class')}; })();").expect("tool preview geometry must execute");
        control.screenshot_to(path).expect("tool preview screenshot must be captured");
        let mut decoder = png::Decoder::new(Cursor::new(fs::read(path).expect("tool PNG must be readable")));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().expect("tool PNG must decode");
        let mut pixels = vec![0; reader.output_buffer_size().expect("tool PNG size must exist")];
        let info = reader.next_frame(&mut pixels).expect("tool PNG pixels must decode");
        let channels = info.color_type.samples();
        let viewport_width = geometry["viewport"]["width"].as_f64().unwrap_or(0.0);
        let viewport_height = geometry["viewport"]["height"].as_f64().unwrap_or(0.0);
        let rect = &geometry["rect"];
        let mut colors = std::collections::BTreeSet::new();
        if viewport_width > 0.0
            && viewport_height > 0.0
            && rect["width"].as_f64().unwrap_or(0.0) > 0.0
            && rect["height"].as_f64().unwrap_or(0.0) > 0.0
        {
            for row in 0..25 {
                for col in 0..25 {
                    let x = rect["x"].as_f64().unwrap_or(0.0) + rect["width"].as_f64().unwrap_or(0.0) * (0.25 + f64::from(col) / 48.0);
                    let y = rect["y"].as_f64().unwrap_or(0.0) + rect["height"].as_f64().unwrap_or(0.0) * (0.25 + f64::from(row) / 48.0);
                    if !(0.0..viewport_width).contains(&x) || !(0.0..viewport_height).contains(&y) {
                        continue;
                    }
                    let px = (x / viewport_width * f64::from(info.width)).floor() as usize;
                    let py = (y / viewport_height * f64::from(info.height)).floor() as usize;
                    let offset = (py * info.width as usize + px) * channels;
                    let rgb = match info.color_type {
                        png::ColorType::Rgb | png::ColorType::Rgba => [pixels[offset], pixels[offset + 1], pixels[offset + 2]],
                        png::ColorType::Grayscale | png::ColorType::GrayscaleAlpha => [pixels[offset]; 3],
                        png::ColorType::Indexed => panic!("expanded tool PNG must not be indexed"),
                    };
                    colors.insert(rgb);
                }
            }
        }
        let visible = geometry["complete"].as_bool() == Some(true)
            && geometry["naturalWidth"].as_u64().is_some_and(|width| width > 0)
            && geometry["visibility"].as_str() == Some("visible")
            && geometry["display"].as_str() != Some("none")
            && geometry["opacity"].as_str().and_then(|value| value.parse::<f64>().ok()).is_some_and(|opacity| opacity > 0.0)
            && colors.len() >= 16;
        last = json!({"dom": geometry, "sampledRgbColors": colors.len(), "visible": visible});
        fs::write(path.with_extension("json"), serde_json::to_vec_pretty(&last).expect("tool receipt must serialize"))
            .expect("tool receipt must save");
        if visible {
            return;
        }
        sleep(Duration::from_millis(100));
    }
    panic!("tool preview never became visible in actual native screenshot: {last}");
}

fn assert_active_grid_is_bounded(control: &rightkit_qa::control::Control) {
    let value = control
        .eval(
            "return (() => { const grid = document.querySelector('.lc-grid-window'); const images = [...document.querySelectorAll('.lc-photo-preview')]; const active = document.querySelectorAll('.lc-photo-cell.is-active'); return {cells: grid?.querySelectorAll('.lc-photo-cell').length || 0, images: images.length, active: active.length, loaded: images.filter((img) => img.complete && img.naturalWidth > 0 && img.naturalHeight > 0).length}; })();",
        )
        .expect("bounded grid DOM query must execute");
    assert!(value["cells"].as_u64().is_some_and(|count| count <= 512), "virtualized grid rendered too many cells: {value}");
    assert!(value["images"].as_u64().is_some_and(|count| count <= 512), "virtualized grid rendered too many images: {value}");
    assert_eq!(value["active"].as_u64(), Some(1), "grid must expose exactly one active photo: {value}");
    assert!(value["loaded"].as_u64().is_some_and(|count| count > 0), "grid must expose decoded WebView pixels: {value}");
}

fn assert_grid_geometry_retains_focus_and_preview(control: &rightkit_qa::control::Control) {
    let nonce = RETAINED_LAYOUT_IDENTITY_NONCE.fetch_add(1, Ordering::Relaxed);
    let key = format!("__lcGridGeometryIdentity_{nonce}");
    let owner = format!("grid-geometry:{nonce}");
    click_dom(control, ".lc-display-wrap > button", "grid geometry display menu must open");
    let setup = control
        .eval(&format!("return (() => {{ const key = {key:?}; const owner = {owner:?}; if (Object.prototype.hasOwnProperty.call(window, key)) return {{owned:false, owner, error:'probe state collision'}}; const cell = document.querySelector('.lc-photo-cell.is-active'); const preview = cell?.querySelector('img.lc-photo-preview'); const frame = cell?.querySelector('.photo-preview-frame'); const grid = document.querySelector('.lc-grid-window'); const input = document.querySelector('.lc-display-size input[type=range]'); if (!cell || !preview || !frame || !grid || !input || !preview.complete || preview.naturalWidth < 1 || preview.naturalHeight < 1) return {{owned:false, owner, error:'focused cell, preview frame, grid or slider missing'}}; cell.focus(); if (document.activeElement !== cell) return {{owned:false, owner, error:'active grid cell could not receive focus'}}; const next = Number(input.value) >= Number(input.max) ? Number(input.min) : Number(input.value) + Number(input.step || 4); window[key] = {{owner, cell, preview, frame, grid, input, next}}; const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set; if (!setter) return {{owned:false, owner, error:'native range value setter missing'}}; setter.call(input, String(next)); input.dispatchEvent(new Event('input', {{bubbles:true}})); input.dispatchEvent(new Event('change', {{bubbles:true}})); return {{owned:true, owner, next, focused:document.activeElement === cell}}; }})();"))
        .expect("grid geometry identity probe must execute");
    assert_eq!(setup["owned"].as_bool(), Some(true), "grid geometry identity probe must initialize: {setup}");
    let mut settled = Value::Null;
    for _ in 0..100 {
        settled = control
            .eval(&format!(
                "return (() => {{ const state = window[{key:?}]; const grid = document.querySelector('.lc-grid-window'); const activeCell = document.querySelector('.lc-photo-cell.is-active'); const currentFrame = state?.cell?.querySelector('.photo-preview-frame'); const currentPreview = state?.cell?.querySelector('img.lc-photo-preview'); const valueReady = Boolean(state && state.owner === {owner:?} && state.input && Number(state.input.value) === state.next); const gridChanged = Boolean(state && grid && grid !== state.grid); const focusSame = Boolean(state && document.activeElement === state.cell); const cellSame = Boolean(state?.cell?.isConnected && activeCell === state.cell); const frameSame = Boolean(state?.frame?.isConnected && currentFrame === state.frame); const imageChanged = Boolean(state && currentPreview && state.preview !== currentPreview); const previewReady = Boolean(currentPreview && currentPreview.complete && currentPreview.naturalWidth > 0 && currentPreview.naturalHeight > 0); return {{ready:valueReady && gridChanged && focusSame && cellSame && frameSame && previewReady, valueReady, gridChanged, focusSame, cellSame, frameSame, imageChanged, previewReady}}; }})();"
            ))
            .expect("grid geometry identity settle query must execute");
        if settled["ready"].as_bool() == Some(true) {
            break;
        }
        sleep(Duration::from_millis(50));
    }
    assert_eq!(settled["ready"].as_bool(), Some(true), "grid geometry identity did not settle: {settled}");
    control.key("Escape").expect("grid geometry display menu dismissal must execute");
    let cleanup = control
        .eval(&format!("return (() => {{ const key = {key:?}; const owner = {owner:?}; const state = window[key]; if (!state) return {{owned:false, owner, error:'probe state missing'}}; if (state.owner !== owner) return {{owned:false, owner, error:'probe state ownership mismatch'}}; delete window[key]; return {{owned:true, owner, deleted:!Object.prototype.hasOwnProperty.call(window, key)}}; }})();"))
        .expect("grid geometry identity probe cleanup must execute");
    assert_eq!(cleanup["owned"].as_bool(), Some(true), "grid geometry identity probe cleanup must be owned: {cleanup}");
    assert_eq!(cleanup["deleted"].as_bool(), Some(true), "grid geometry identity probe cleanup must delete state: {cleanup}");
}

fn grid_metrics(control: &rightkit_qa::control::Control) -> Value {
    control.eval(r#"return (() => {
        const scroll = document.querySelector('.lc-grid-scroll');
        const grid = document.querySelector('.lc-grid-window');
        const cells = [...(grid?.querySelectorAll('.lc-photo-cell') || [])];
        const images = [...(grid?.querySelectorAll('img.lc-photo-preview') || [])];
        const first = grid?.querySelector('.lc-photo-caption span:first-child')?.textContent?.trim() || '';
        const box = e => { if (!e) return null; const r=e.getBoundingClientRect(),s=getComputedStyle(e); return {x:r.x,y:r.y,width:r.width,height:r.height,bottom:r.bottom,display:s.display,position:s.position,style:e.style.cssText}; };
        const bounds=scroll?.getBoundingClientRect();
        const visible=cells.filter(e=>{const r=e.getBoundingClientRect();return bounds&&r.bottom>bounds.top&&r.top<bounds.bottom;});
        const ready=visible.filter(e=>[...e.querySelectorAll('img.lc-photo-preview')].some(img=>img.complete&&img.naturalWidth>0&&img.naturalHeight>0));
        const describe=e=>({name:e.querySelector('.lc-photo-name')?.textContent,box:box(e),row:box(e.parentElement),error:e.querySelector('.photo-preview-frame')?.getAttribute('aria-label')});
        return {visible:visible.length,visibleReady:ready.length,errors:grid?.querySelectorAll('[data-preview-state=error]').length||0,scrollTop:scroll?.scrollTop||0,scrollHeight:scroll?.scrollHeight||0,clientHeight:scroll?.clientHeight||0,top:Number.parseFloat(grid?.style.top||'0')||0,cells:cells.length,images:images.length,loaded:images.filter(img=>img.complete&&img.naturalWidth>0&&img.naturalHeight>0).length,first,busy:grid?.getAttribute('aria-busy')==='true',scrollBox:box(scroll),gridBox:box(grid),firstCells:cells.slice(0,5).map(describe),lastCells:cells.slice(-5).map(describe)};
    })();"#).expect("scalable grid DOM query must execute")
}

fn layout_style_diagnostics(control: &rightkit_qa::control::Control) -> Value {
    control.eval(r#"return (() => {
        const rules=[];
        const visit=(list,chain=[])=>{for(const rule of list){if(rule.selectorText&&/lc-stage-layout|lc-library-layout|lc-grid-window|lc-grid-row/.test(rule.selectorText))rules.push({chain,selector:rule.selectorText,style:rule.style.cssText});if(rule.cssRules)visit(rule.cssRules,chain.concat(rule.conditionText||rule.name||rule.cssText.slice(0,100)));}};
        for(const sheet of document.styleSheets){try{visit(sheet.cssRules,[sheet.href]);}catch(error){rules.push({href:sheet.href,error:String(error)});}}
        const geometry=e=>{if(!e)return null;const r=e.getBoundingClientRect(),s=getComputedStyle(e);return {className:e.className,x:r.x,y:r.y,width:r.width,height:r.height,top:s.top,columns:s.gridTemplateColumns,inline:e.style.cssText};};
        const layout=document.querySelector('.lc-stage-layout.is-inspector-collapsed,.lc-library-layout.is-inspector-collapsed');
        let collapsedInlineProbe=null;
        if(layout){const property='grid-template-columns',original=layout.style.getPropertyValue(property),priority=layout.style.getPropertyPriority(property);const before=geometry(layout);try{layout.style.setProperty(property,'minmax(360px,1fr) 0px 44px');collapsedInlineProbe={before,inline:geometry(layout)};}finally{if(original)layout.style.setProperty(property,original,priority);else layout.style.removeProperty(property);}collapsedInlineProbe.restored=geometry(layout);}
        const grid=document.querySelector('.lc-grid-window');
        let gridTopProbe=null;
        if(grid){const original=grid.style.getPropertyValue('top'),priority=grid.style.getPropertyPriority('top'),before=geometry(grid);try{grid.style.setProperty('top','0px');gridTopProbe={before,zero:geometry(grid)};}finally{if(original)grid.style.setProperty('top',original,priority);else grid.style.removeProperty('top');}gridTopProbe.restored=geometry(grid);}
        return {rules,collapsedInlineProbe,gridTopProbe,documentHidden:document.hidden,visibilityState:document.visibilityState,unitlessZeroTrack:CSS.supports('grid-template-columns','minmax(360px,1fr) 0 44px'),pixelZeroTrack:CSS.supports('grid-template-columns','minmax(360px,1fr) 0px 44px'),userAgent:navigator.userAgent,previewErrors:[...document.querySelectorAll('.photo-preview-frame[aria-label]')].map(e=>e.getAttribute('aria-label')).slice(0,100)};
    })();"#).unwrap_or_else(|error| json!({"error": error.to_string()}))
}

fn wait_for_grid(control: &rightkit_qa::control::Control, scrolled: bool, previous_first: Option<&str>) -> Value {
    for _ in 0..200 {
        let value = grid_metrics(control);
        let visible = value["cells"].as_u64().is_some_and(|count| count <= 512)
            && value["images"].as_u64().is_some_and(|count| count <= 512)
            && value["loaded"].as_u64().is_some_and(|count| count > 0)
            && value["visible"].as_u64().is_some_and(|count| count > 0)
            && value["visibleReady"] == value["visible"]
            && value["errors"].as_u64() == Some(0)
            && value["busy"].as_bool() == Some(false)
            && previous_first.is_none_or(|previous| value["first"].as_str() != Some(previous))
            && (!scrolled || (value["scrollTop"].as_f64().unwrap_or(0.0) > 0.0 && value["top"].as_f64().unwrap_or(0.0) > 0.0));
        if visible {
            return value;
        }
        sleep(Duration::from_millis(50));
    }
    panic!("scalable grid did not settle: {:?}", grid_metrics(control));
}

fn assert_thumbnail_page_boundaries(control: &rightkit_qa::control::Control) {
    click_dom(control, ".lc-display-wrap > button", "Display menu must open for square grid");
    click_dom(control, ".lc-display-views button:last-child", "square grid must open");
    click_dom(control, ".lc-display-wrap > button", "Display menu must close after grid choice");
    wait_for_dom(control, "return document.querySelector('.lc-grid-scroll.is-square') !== null;");
    wait_for_grid(control, false, None);
    control.eval("return (()=>{const s=document.querySelector('.lc-grid-scroll'),g=document.querySelector('.lc-grid-window'),cell=g.querySelector('.lc-photo-cell');const cols=getComputedStyle(g).gridTemplateColumns.split(' ').length,stride=cell.getBoundingClientRect().height+parseFloat(getComputedStyle(g).rowGap);s.scrollTop=(Math.floor(512/cols)-1)*stride;s.dispatchEvent(new Event('scroll',{bubbles:true}));return true;})();").expect("square grid page-boundary scroll must execute");
    wait_for_grid(control, true, None);
    wait_for_dom(
        control,
        "return (()=>{const s=document.querySelector('.lc-grid-scroll'),r=s.getBoundingClientRect(),cells=[...s.querySelectorAll('.lc-photo-cell')].map(e=>e.getBoundingClientRect()).filter(c=>c.bottom>r.top&&c.top<r.bottom);return cells.length>0&&Math.min(...cells.map(c=>c.top))<=r.top+14&&Math.max(...cells.map(c=>c.bottom))>=r.bottom-18;})();",
    );
    control.eval("return (()=>{const s=document.querySelector('.lc-filmstrip-scroll');s.scrollLeft=127*132;s.dispatchEvent(new Event('scroll',{bubbles:true}));return true;})();").expect("filmstrip page-boundary scroll must execute");
    wait_for_dom(
        control,
        "return (()=>{const s=document.querySelector('.lc-filmstrip-scroll'),r=s.getBoundingClientRect(),items=[...s.querySelectorAll('.lc-filmstrip-item')].filter(e=>{const b=e.getBoundingClientRect();return b.right>r.left&&b.left<r.right;});return items.length>0&&items.every(e=>[...e.querySelectorAll('img')].some(i=>i.complete&&i.naturalWidth>0))&&Math.min(...items.map(e=>e.getBoundingClientRect().left))<=r.left+14&&Math.max(...items.map(e=>e.getBoundingClientRect().right))>=r.right-14&&!s.querySelector('[data-preview-state=error]');})();",
    );
    control.eval("return (()=>{for(const [selector,axis] of [['.lc-filmstrip-scroll','scrollLeft'],['.lc-grid-scroll','scrollTop']]){const e=document.querySelector(selector);e[axis]=0;e.dispatchEvent(new Event('scroll',{bubbles:true}));}return true;})();").expect("thumbnail boundary scroll must restore start");
    click_dom(control, ".lc-display-wrap > button", "Display menu must reopen");
    click_dom(control, ".lc-display-views button:first-child", "photo grid must restore");
    click_dom(control, ".lc-display-wrap > button", "Display menu must close after restoring grid");
    wait_for_grid(control, false, None);
}

fn enable_grid_info(control: &rightkit_qa::control::Control) {
    click_dom(control, ".lc-display-wrap > button", "Display menu must open");
    let label = control.eval("return [...document.querySelectorAll('.lc-display-menu [role=menuitem]')].find(node => node.textContent?.startsWith('Grid info:'))?.textContent || ''; ").expect("Grid info state must be queryable");
    assert!(label.as_str().is_some_and(|text| text.starts_with("Grid info:")), "Grid info control must exist inside Display menu: {label}");
    if label.as_str().is_some_and(|text| text.ends_with("Off")) {
        click_dom(control, ".lc-display-menu > button[role=menuitem]", "Grid info must enable via Display menu");
    }
    click_dom(control, ".lc-display-wrap > button", "Display menu must close");
    wait_for_dom(control, "return Boolean(document.querySelector('.lc-grid-window .lc-photo-caption span:first-child')?.textContent?.trim());");
}

fn scroll_grid_to_end(control: &rightkit_qa::control::Control) {
    let changed = control
        .eval("return (() => { const scroll = document.querySelector('.lc-grid-scroll'); if (!scroll) return false; scroll.scrollTop = Math.max(0, scroll.scrollHeight - scroll.clientHeight); scroll.dispatchEvent(new Event('scroll', {bubbles: true})); return scroll.scrollHeight > scroll.clientHeight; })();")
        .expect("grid scroll command must execute");
    assert_eq!(changed.as_bool(), Some(true), "scalable grid must have scrollable height");
}

fn assert_active_grid_identity(control: &rightkit_qa::control::Control, file_name: &str) {
    let value = control
        .eval("return document.querySelector('.lc-photo-cell.is-active .lc-photo-caption span:first-child')?.textContent?.trim() || '';")
        .expect("active grid identity query must execute");
    assert_eq!(value.as_str(), Some(file_name), "active grid DOM identity must match selected source");
}

fn wait_for_active_grid_preview(control: &rightkit_qa::control::Control, file_name: &str, expected_src: Option<&str>) -> Value {
    let mut last = Value::Null;
    let expression = "return (() => { const img = document.querySelector('.lc-photo-cell.is-active img.lc-photo-preview'); if (!img) return {ready:false, reason:'missing'}; const cell = img.closest('.lc-photo-cell'); const src = img.getAttribute('src') || ''; const identity = cell?.querySelector('.lc-photo-caption span:first-child')?.textContent?.trim() || ''; return {ready: img.complete && img.naturalWidth > 0 && img.naturalHeight > 0, naturalWidth: img.naturalWidth, naturalHeight: img.naturalHeight, src, identity, active: Boolean(cell?.classList.contains('is-active')), selected: Boolean(cell?.getAttribute('aria-selected') === 'true')}; })();";
    for _ in 0..160 {
        let value = control.eval(expression).expect("active grid preview DOM query must execute");
        let ready = value["ready"].as_bool() == Some(true);
        let scoped = value["src"].as_str().is_some_and(is_scoped_preview_src);
        let changed = expected_src.is_none_or(|previous| value["src"].as_str() != Some(previous));
        let identity = value["identity"].as_str() == Some(file_name);
        let active = value["active"].as_bool() == Some(true);
        let selected = value["selected"].as_bool() == Some(true);
        if ready && scoped && changed && identity && active && selected {
            assert_eq!(value["active"].as_bool(), Some(true), "active grid cell must own rendered preview: {value}");
            assert_eq!(value["selected"].as_bool(), Some(true), "active grid cell must be selected: {value}");
            assert!(value["naturalWidth"].as_u64().is_some_and(|width| width > 0));
            assert!(value["naturalHeight"].as_u64().is_some_and(|height| height > 0));
            return value;
        }
        last = value;
        sleep(Duration::from_millis(50));
    }
    panic!("active grid preview did not settle on {file_name}: {expression}; last result={last}");
}

fn wait_task(control: &rightkit_qa::control::Control, task_id: &str) -> Value {
    for _ in 0..900 {
        let value = snapshot(control);
        if let Some(completed) =
            value["status"]["completedJobs"].as_array().and_then(|jobs| jobs.iter().find(|job| job["id"].as_str() == Some(task_id)))
        {
            match completed["state"].as_str() {
                Some("done") => return value,
                Some("cancelled") => panic!("native task {task_id} was cancelled: {completed}"),
                Some("failed") => panic!("native task {task_id} failed: {completed}"),
                state => panic!("native task {task_id} has unknown terminal state {state:?}: {completed}"),
            }
        }
        sleep(Duration::from_millis(100));
    }
    panic!("native task {task_id} did not publish exact completedJobs receipt within 90 seconds");
}

fn task_result(control: &rightkit_qa::control::Control, id: &str, params: Value) -> Value {
    let started = run(control, id, params);
    let task_id = started["taskId"].as_str().expect("background command must return task id");
    let state = wait_task(control, task_id);
    state["status"]["completedJobs"]
        .as_array()
        .expect("completed task history must exist")
        .iter()
        .find(|job| job["id"].as_str() == Some(task_id))
        .expect("exact task must be retained")["result"]
        .clone()
}

fn import_review(control: &rightkit_qa::control::Control, paths: Value) -> Value {
    let before = snapshot(control);
    let started = run(control, "library.importPreview", paths);
    let task_id = started["taskId"].as_str().expect("import review must return task id").to_string();
    assert_eq!(started["kind"].as_str(), Some("importReview"), "import review must identify task kind: {started}");
    let during = snapshot(control);
    assert!(
        during["status"]["jobs"].as_array().is_some_and(|jobs| jobs.iter().any(|job| job["id"].as_str().is_some_and(|id| id == task_id)))
            || during["status"]["completedJobs"]
                .as_array()
                .is_some_and(|jobs| jobs.iter().any(|job| job["id"].as_str().is_some_and(|id| id == task_id))),
        "import review must publish task without blocking snapshot access: {during}"
    );
    assert_eq!(during["active"], before["active"], "import review must not change active photo while running");
    assert_eq!(during["selection"], before["selection"], "import review must not change selection while running");
    let finished = wait_task(control, &task_id);
    let receipt = completed_task(&finished, &task_id);
    assert_eq!(receipt["state"].as_str(), Some("done"), "import review must complete successfully: {receipt}");
    for key in ["active", "selection", "counts", "undo", "redo", "revision", "viewGeneration", "source"] {
        assert_eq!(finished[key], before[key], "import review must leave owner {key} unchanged: {finished}");
    }
    receipt["result"].clone()
}

fn import_file(control: &rightkit_qa::control::Control, path: &Path) -> Value {
    let preview = import_review(control, json!({"paths": [path]}));
    assert!(preview["candidates"].as_array().is_some_and(|items| !items.is_empty()), "real fixture must produce an import candidate: {preview}");
    let started = run(control, "library.import", json!({"paths": [path], "mode": "add"}));
    let task_id = started["taskId"].as_str().expect("import must return task id").to_string();
    let snapshot = wait_task(control, &task_id);
    assert!(snapshot["counts"]["catalog"].as_u64().is_some_and(|count| count > 0), "import must add catalog photo: {snapshot}");
    snapshot
}

fn write_merge_fixture_copies(root: &Path, source: &Path) -> [PathBuf; 2] {
    let first = root.join("merge-procedural-a.png");
    let second = root.join("merge-procedural-b.png");
    fs::copy(source, &first).unwrap_or_else(|error| panic!("copy first procedural merge PNG: {error}"));

    let mut decoder = png::Decoder::new(Cursor::new(fs::read(source).expect("procedural PNG must be readable")));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().expect("procedural PNG must decode");
    let mut pixels = vec![0; reader.output_buffer_size().expect("procedural PNG buffer must exist")];
    let info = reader.next_frame(&mut pixels).expect("procedural PNG pixels must decode");
    assert_eq!(info.color_type, png::ColorType::Rgba, "procedural merge source must decode to RGBA");
    for pixel in pixels[..info.buffer_size()].as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let mut encoded = Vec::new();
    let mut encoder = png::Encoder::new(&mut encoded, info.width, info.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = encoder.write_header().expect("second merge PNG header must encode");
    writer.write_image_data(&pixels[..info.buffer_size()]).expect("second merge PNG pixels must encode");
    drop(writer);
    fs::write(&second, encoded).expect("second procedural merge PNG must be writable");
    assert_ne!(source_hash(&first), source_hash(&second), "merge inputs must be distinct procedural PNG content");
    [first, second]
}

fn completed_task<'a>(snapshot: &'a Value, task_id: &str) -> &'a Value {
    snapshot["status"]["completedJobs"]
        .as_array()
        .and_then(|jobs| jobs.iter().find(|job| job["id"].as_str() == Some(task_id)))
        .unwrap_or_else(|| panic!("completedJobs must contain exact task {task_id}: {snapshot}"))
}

fn preview_imported(control: &rightkit_qa::control::Control, snapshot: &Value) -> Value {
    let photo_id = snapshot["active"].as_u64().expect("import must select active photo");
    let request = json!({
        "photoId": photo_id,
        "slot": "qualification",
        "viewGeneration": snapshot["viewGeneration"],
        "width": 96,
        "height": 64,
        "quality": "draft",
        "before": false,
        "sequence": 1,
    });
    let descriptor = control.command("lc_preview", &json!({"request": request})).expect("real preview must render");
    assert!(descriptor["handle"].as_str().is_some_and(|handle| !handle.is_empty()), "preview must return stored handle: {descriptor}");
    assert_eq!(descriptor["viewGeneration"], snapshot["viewGeneration"]);
    descriptor
}

fn assert_png_pixels(path: &Path, expected_width: u32, expected_height: u32) {
    let bytes = fs::read(path).expect("exported PNG must be readable");
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder.read_info().expect("exported PNG must decode");
    let mut pixels = vec![0; reader.output_buffer_size().expect("PNG output buffer size must be available")];
    let info = reader.next_frame(&mut pixels).expect("exported PNG frame must decode");
    assert_eq!(info.width, expected_width, "exported PNG width must match requested long edge");
    assert_eq!(info.height, expected_height, "exported PNG height must match source aspect");
    assert!(info.buffer_size() > 0, "exported PNG must contain decoded pixels");
    assert!(pixels[..info.buffer_size()].iter().any(|&pixel| pixel != 0), "exported PNG pixels must not be all zero");
}

fn set_dialog_field(control: &rightkit_qa::control::Control, label: &str, value: &str) {
    let label_json = serde_json::to_string(label).expect("field label must serialize");
    let value_json = serde_json::to_string(value).expect("field value must serialize");
    let changed = control.eval(&format!(r#"return (() => {{
        const field = [...document.querySelectorAll('.lc-dialog .lc-field')].find((node) => node.querySelector('span')?.textContent === {label_json});
        const node = field?.querySelector('input,select,textarea');
        if (!node) return false;
        const prototype = node instanceof HTMLSelectElement ? HTMLSelectElement.prototype : node instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
        Object.getOwnPropertyDescriptor(prototype, 'value').set.call(node, {value_json});
        node.dispatchEvent(new Event(node instanceof HTMLSelectElement ? 'change' : 'input', {{bubbles:true}}));
        return node.value === {value_json};
    }})();"#)).expect("dialog field input must execute in native WebView");
    assert_eq!(changed.as_bool(), Some(true), "native Export field {label} must accept input");
    wait_for_dom(
        control,
        &format!(
            "return [...document.querySelectorAll('.lc-dialog .lc-field')].find((node) => node.querySelector('span')?.textContent === {label_json})?.querySelector('input,select,textarea')?.value === {value_json};"
        ),
    );
}

fn top_left_magenta_pixels(path: &Path) -> usize {
    let mut decoder = png::Decoder::new(Cursor::new(fs::read(path).expect("watermark PNG must be readable")));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().expect("watermark PNG must decode");
    let mut pixels = vec![0; reader.output_buffer_size().expect("watermark buffer must be available")];
    let info = reader.next_frame(&mut pixels).expect("watermark pixels must decode");
    let channels = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        _ => panic!("rendered watermark PNG must contain RGB channels"),
    };
    pixels[..info.buffer_size()]
        .chunks_exact(channels)
        .enumerate()
        .filter(|(index, pixel)| {
            let x = *index % info.width as usize;
            let y = *index / info.width as usize;
            x < info.width as usize / 2 && y < info.height as usize / 2 && pixel[0] > 240 && pixel[1] < 20 && pixel[2] > 240
        })
        .count()
}

fn tree_fingerprint(path: &Path) -> String {
    fn walk(root: &Path, path: &Path, rows: &mut Vec<String>) {
        let mut entries =
            fs::read_dir(path).expect("backup tree must be readable").collect::<Result<Vec<_>, _>>().expect("backup entries must be readable");
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let child = entry.path();
            if child.is_dir() {
                walk(root, &child, rows);
            } else {
                let bytes = fs::read(&child).expect("backup file must be readable");
                rows.push(format!("{}:{}", child.strip_prefix(root).unwrap_or(&child).display(), sha256_hex(&bytes)));
            }
        }
    }
    let mut rows = Vec::new();
    walk(path, path, &mut rows);
    sha256_hex(rows.join("\n").as_bytes())
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("copy destination must be created");
    let entries = fs::read_dir(source).expect("copy source must be readable").collect::<Result<Vec<_>, _>>().expect("copy entries must be readable");
    for entry in entries {
        let from = entry.path();
        let to = destination.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap_or_else(|error| panic!("copy {}: {error}", from.display()));
        }
    }
}

fn assert_native_window_hidden(control: &rightkit_qa::control::Control) {
    let state = control.command("lc_qa_window_state", &Value::Null).expect("native QA window state must reply");
    assert_eq!(state["visible"].as_bool(), Some(false), "native QA window must stay invisible: {state}");
    assert_eq!(state["focused"].as_bool(), Some(false), "native QA window must never receive OS focus: {state}");
}

fn with_control<T>(
    binary: &Path,
    scenario: &rightkit_qa::harness::Scenario,
    catalog: &Path,
    body: impl FnOnce(&rightkit_qa::control::Control, &Path) -> T,
) -> T {
    let (mut control, data) = launch_hidden(binary, scenario, catalog);
    let result = catch_unwind(AssertUnwindSafe(|| {
        assert_native_window_hidden(&control);
        let value = body(&control, &data);
        assert_native_window_hidden(&control);
        value
    }));
    eprintln!("[qa] journey body: name={} passed={}", scenario.name(), result.is_ok());
    if result.is_err() {
        let layout = layout_style_diagnostics(&control);
        let path = scenario.dir().join("failure-layout.json");
        if let Ok(bytes) = serde_json::to_vec_pretty(&layout)
            && fs::write(&path, bytes).is_ok()
        {
            scenario.keep("failure-layout.json", &path);
        }
        let state = control.eval("return {url: location.href, title: document.title, width: window.innerWidth, height: window.innerHeight, rootChildren: document.getElementById('root')?.childElementCount, layout: ['html', 'body', '#root', '.rk-shell', '.rk-body', '.rk-plane', '.lc-content', '.lc-library-layout', '.lc-library-center', '.lc-library-workspace', '.lc-stage-layout', '.lc-inspector', '.lc-inspector__rail'].map(selector => { const e = document.querySelector(selector); if (!e) return {selector, missing: true}; const r = e.getBoundingClientRect(), s = getComputedStyle(e); return {selector, className: e.className, rect: {x: r.x, y: r.y, width: r.width, height: r.height, right: r.right}, display: s.display, rows: s.gridTemplateRows, columns: s.gridTemplateColumns, visibility: s.visibility}; }), activeElement: document.activeElement?.outerHTML.slice(0, 500), preview: document.querySelector('.stage-preview')?.outerHTML, previewStates: Array.from(document.querySelectorAll('[data-preview-state]')).map(node => ({state: node.getAttribute('data-preview-state'), html: node.outerHTML.slice(0, 1000)})), body: document.body.innerText.slice(0, 4000)};");
        eprintln!("[qa] failure DOM={state:?}");
        if let Ok(state) = state {
            let path = scenario.dir().join("failure-dom.json");
            if let Ok(bytes) = serde_json::to_vec_pretty(&state)
                && fs::write(&path, bytes).is_ok()
            {
                scenario.keep("failure-dom.json", &path);
            }
        }
        let path = scenario.dir().join("failure-native.png");
        match control.screenshot_to(&path) {
            Ok(_) => {
                scenario.keep("failure-native.png", &path);
            }
            Err(error) => eprintln!("[qa] failure screenshot unavailable: {error}"),
        }
        if cfg!(target_os = "macos")
            && let Ok(viewport) = control.eval("return {width: window.innerWidth, height: window.innerHeight};")
            && let (Some(width), Some(height)) = (viewport["width"].as_u64(), viewport["height"].as_u64())
            && let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height))
        {
            let prior_probe_changed_parent = diagnose_stage_child_invalidation(&control, scenario);
            diagnose_same_parent_layout(&control, scenario, prior_probe_changed_parent);
            diagnose_native_viewport_bounce(&control, scenario, width, height);
        }
    }
    let stopped = control.stop().expect("native app must stop cleanly");
    assert!(stopped.endpoint_closed, "rightkit-control endpoint must close");
    assert!(stopped.process_gone, "native app process must exit");
    if cfg!(target_os = "macos") {
        assert!(
            stopped.owned_never_frontmost,
            "hidden qualification must never activate app: states={:?}, owned_frontmost_pids={:?}, before={:?}, after={:?}",
            stopped.frontmost_states, stopped.owned_frontmost_pids, stopped.frontmost_before, stopped.frontmost_after,
        );
    }
    match result {
        Ok(value) => value,
        Err(error) => resume_unwind(error),
    }
}

fn run_catalog_recovery(
    binary: &Path,
    scenario: &rightkit_qa::harness::Scenario,
    catalog: &Path,
    png: &Path,
    baseline_capture: &Path,
    source_revision: &str,
    installed_hash: &str,
) {
    let (before_counts, active, expected_exposure, backup, backup_fingerprint) = with_control(binary, scenario, catalog, |control, _data| {
        let before = import_file(control, png);
        let active = before["active"].as_u64().expect("import must select active photo");
        run(control, "develop.beginInteraction", json!({"label": "Backup baseline edit"}));
        run(control, "develop.set", json!({"control": "light.exposure", "value": 1.25, "ids": [active]}));
        run(control, "develop.endInteraction", json!({}));
        let edited = snapshot(control);
        let before_counts = edited["counts"].clone();
        let expected_exposure = edited["develop"]["light"]["exposure"].as_f64().expect("edited exposure must be numeric");
        let backup = scenario.dir().join("library-backup");
        let backup_result = run(control, "library.backup", json!({"path": backup}));
        assert!(backup_result["path"].as_str().is_some(), "backup must return destination: {backup_result}");
        assert!(backup.is_dir(), "backup directory must exist");
        let backup_fingerprint = tree_fingerprint(&backup);
        (before_counts, active, expected_exposure, backup, backup_fingerprint)
    });

    with_control(binary, scenario, catalog, |control, _data| {
        let current = snapshot(control);
        assert_eq!(current["counts"]["catalog"].as_u64(), before_counts["catalog"].as_u64(), "current catalog must reopen before mutation");
        let current_active = current["active"].as_u64().expect("reopened catalog must select active photo");
        run(control, "develop.beginInteraction", json!({"label": "Mutate before restore"}));
        run(control, "develop.set", json!({"control": "light.exposure", "value": -1.25, "ids": [current_active]}));
        run(control, "develop.endInteraction", json!({}));
        let mutated = snapshot(control);
        assert_ne!(mutated["develop"]["light"]["exposure"].as_f64(), Some(expected_exposure), "current catalog must differ before restore");
    });
    assert_eq!(tree_fingerprint(&backup), backup_fingerprint, "backup must remain unchanged while current catalog is mutated");

    with_control(binary, scenario, catalog, |control, _data| {
        let restored = run(control, "library.restore", json!({"path": backup}));
        let backup_text = backup.to_string_lossy().to_string();
        let source_backup = restored["sourceBackup"].as_str().expect("restore must report original source backup");
        let restored_path = restored["restoredPath"].as_str().expect("restore must report copied restored path");
        assert_eq!(source_backup, backup_text, "restore sourceBackup must identify original backup");
        assert_eq!(restored["libraryPath"].as_str(), Some(restored_path), "restore libraryPath must identify copied restored path");
        assert_ne!(source_backup, restored_path, "restore must open a fresh owned copy");
        assert_eq!(tree_fingerprint(&backup), backup_fingerprint, "backup must remain unchanged immediately after restore");
        let after = snapshot(control);
        assert_eq!(after["counts"], before_counts, "restore must recover catalog counts");
        assert_eq!(after["active"].as_u64(), Some(active), "restore must recover active photo identity");
        assert_eq!(after["develop"]["light"]["exposure"].as_f64(), Some(expected_exposure), "restore must recover edited photo state");

        let open_library = scenario.dir().join("open-library");
        copy_tree(&backup, &open_library);
        let generation_before_open = after["viewGeneration"].as_u64().expect("restore generation must be numeric");
        let opened = control
            .command("lc_native", &json!({"action": "openLibrary", "params": {"path": open_library}}))
            .expect("native openLibrary command must execute");
        let open_library_text = open_library.to_string_lossy().to_string();
        assert_eq!(opened["path"].as_str(), Some(open_library_text.as_str()), "openLibrary must report copied path");
        let opened_snapshot = snapshot(control);
        assert_eq!(opened_snapshot["libraryPath"].as_str(), Some(open_library_text.as_str()), "snapshot must expose newly opened library path");
        assert!(
            opened_snapshot["viewGeneration"].as_u64().is_some_and(|generation| generation > generation_before_open),
            "openLibrary must advance view generation"
        );
        control.key("G").expect("library route key must execute after openLibrary");
        wait_for_dom(control, "return document.querySelector('.lc-library-workspace') !== null;");
        wait_for_rendered_preview(control, ".lc-photo-cell.is-active img.lc-photo-preview", None);
        assert_active_grid_identity(control, "procedural-rgb-01.png");
        assert_eq!(tree_fingerprint(&backup), backup_fingerprint, "original backup must remain unchanged after copied openLibrary journey");
        control.screenshot_to(baseline_capture).expect("catalog recovery screenshot must be captured");
        assert!(baseline_capture.is_file());
        let receipt = scenario.dir().join("catalog-recovery.json");
        fs::write(
            &receipt,
            serde_json::to_vec_pretty(&json!({
                "schema": 2,
                "sourceRevision": source_revision,
                "installedArtifactSha256": installed_hash,
                "catalogRecovery": {"qualified": true, "method": "backup-mutated-catalog-restore"},
                "installerRollback": {"qualified": false, "requiresSeparateEvidence": true},
                "beforeCounts": before_counts,
                "afterCounts": after["counts"],
                "backupFingerprint": backup_fingerprint,
                "sourceBackup": source_backup,
                "restoredLibraryPath": restored_path,
                "openLibraryPath": opened_snapshot["libraryPath"],
                "expectedExposure": expected_exposure,
            }))
            .expect("catalog recovery receipt must serialize"),
        )
        .expect("catalog recovery receipt must be writable");
        scenario.keep("catalog-recovery.json", &receipt);
    });
}

#[test]
#[ignore = "target-native candidate qualification only; invoke with cargo test --ignored on CI"]
fn native_hidden_control_journeys() {
    let binary = PathBuf::from(required_env("RIGHTKIT_QA_UI_BINARY"));
    assert!(binary.is_file(), "candidate executable must exist: {}", binary.display());
    let evidence = PathBuf::from(required_env("RIGHTKIT_QA_EVIDENCE"));
    let revision = required_env("RIGHTKIT_QA_SOURCE_REVISION");
    let architecture = required_env("RIGHTKIT_QA_ARCHITECTURE");
    let installed_hash = required_env("RIGHTKIT_QA_INSTALLED_ARTIFACT_SHA256");
    assert_eq!(installed_hash.len(), 64, "installed artifact hash must be SHA-256");
    assert!(installed_hash.bytes().all(|byte| byte.is_ascii_hexdigit()), "installed artifact hash must be hexadecimal");
    assert_eq!(source_hash(&binary), installed_hash, "installed binary must match admitted artifact hash");
    let platform = if cfg!(target_os = "macos") { "macos" } else { "windows" };
    let baseline: Value =
        serde_json::from_str(&fs::read_to_string(fixture_root().join("installed-baseline.json")).expect("baseline fixture must be readable"))
            .expect("baseline fixture must be valid JSON");
    assert_eq!(baseline["catalogRecovery"]["required"].as_bool(), Some(true));
    assert_eq!(baseline["installerRollback"]["qualified"].as_bool(), Some(false));
    let input_dir = evidence.join("fixture-inputs");
    fs::create_dir_all(&input_dir).expect("fixture input directory must exist");
    let inputs = fixture_inputs::write_fixture_inputs(&input_dir).expect("real ARW/PNG/Lightroom fixtures must be generated");
    let merge_inputs = write_merge_fixture_copies(&input_dir, &inputs.png);
    let source_paths = [
        ("arw", inputs.arw.as_path()),
        ("png", inputs.png.as_path()),
        ("catalog", inputs.catalog.as_path()),
        ("scalabilitySources", inputs.scalability_sources.as_path()),
        ("mergePngA", merge_inputs[0].as_path()),
        ("mergePngB", merge_inputs[1].as_path()),
    ];
    let source_hashes = source_paths.map(|(label, path)| (label, source_fingerprint(path)));
    let source_manifest = input_dir.join("manifest.json");
    fs::write(
        &source_manifest,
        serde_json::to_vec_pretty(&json!({
            "schema": 2,
            "generator": "fixture_inputs:v2",
            "sourceRevision": revision,
            "installedArtifactSha256": installed_hash,
            "platform": platform,
            "architecture": architecture,
            "inputs": source_hashes.iter().map(|(label, hash)| json!({"label": label, "sha256": hash})).collect::<Vec<_>>(),
        }))
        .expect("source manifest must serialize"),
    )
    .expect("source manifest must be writable");
    let harness = qa_harness(&binary, &evidence, &revision, platform, &architecture);

    let scenario_names = [
        // Capture core import/edit/undo/export proof before broader parity journeys.
        "engineExport",
        "mergeHdr",
        "cullReview",
        "ipc",
        "stalePreview",
        "cache",
        "scalability",
        "preferences",
        "gesture",
        "editingTools",
        "importFilters",
        "modelSetup",
        "arwImport",
        "lightroomImport",
        "catalogRecovery",
    ];
    let mut failures = Vec::new();
    for name in scenario_names {
        // Keep collecting native evidence after a failed journey. Every failure
        // still fails this test, without hiding later platform defects.
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            harness.scenario(name, "fast", &[], |scenario| {
            let baseline_capture = scenario.dir().join("baseline.png");
            if name == "preferences" {
                with_control(&binary, scenario, &inputs.catalog, |control, _data| {
                    let updated = control
                        .command("lc_preferences", &json!({"qaSentinel": "preserved", "ui": {"theme": "dark", "view": "detail", "startupView": "photoGrid"}}))
                        .expect("preferences patch must reply");
                    assert_eq!(updated["qaSentinel"].as_str(), Some("preserved"));
                });
                with_control(&binary, scenario, &inputs.catalog, |control, _data| {
                    let reopened = control.command("lc_preferences", &Value::Null).expect("preferences reopen must reply after process restart");
                    assert_eq!(reopened["qaSentinel"].as_str(), Some("preserved"), "unknown preference fields must survive process restart");
                    assert_eq!(reopened["ui"]["theme"].as_str(), Some("dark"), "dark theme choice must survive restart");
                    assert_eq!(reopened["ui"]["startupView"].as_str(), Some("photoGrid"), "startup choice must survive restart");
                    wait_for_dom(control, "return document.querySelector('.lc-library-workspace') !== null;");
                    wait_for_dom(control, "return document.documentElement.getAttribute('data-theme') === 'dark';");
                    let dark = scenario.dir().join("dark-theme.png");
                    control.screenshot_to(&dark).expect("dark theme screenshot must be captured");
                    assert!(dark.is_file());
                    assert_library_header_contrast(control, "dark");
                    click_dom(control, ".rk-top button.rk-seg__item[title='Develop']", "Develop segment must select actual detail view");
                    wait_for_dom(control, "return document.querySelector('.stage-detail') !== null && document.querySelector('.lc-library-workspace') === null && document.querySelector('.lc-filmstrip') !== null;");
                    assert_filmstrip_context_contrast(control, "dark");
                    click_dom(control, ".rk-top button.rk-seg__item[title='Library']", "Library segment must restore actual library view");
                    wait_for_dom(control, "return document.querySelector('.lc-library-workspace') !== null;");
                    control.command("lc_preferences", &json!({"ui": {"theme": "light"}})).expect("light theme preference must persist");
                });
                with_control(&binary, scenario, &inputs.catalog, |control, _data| {
                    let reopened = control.command("lc_preferences", &Value::Null).expect("light preference must reopen");
                    assert_eq!(reopened["ui"]["theme"].as_str(), Some("light"), "light theme choice must survive restart");
                    wait_for_dom(control, "return document.documentElement.getAttribute('data-theme') === 'light';");
                    let light = scenario.dir().join("light-theme.png");
                    control.screenshot_to(&light).expect("light theme screenshot must be captured");
                    assert!(light.is_file());
                    assert_library_header_contrast(control, "light");
                    click_dom(control, ".rk-top button.rk-seg__item[title='Develop']", "Develop segment must select actual detail view");
                    wait_for_dom(control, "return document.querySelector('.stage-detail') !== null && document.querySelector('.lc-library-workspace') === null && document.querySelector('.lc-filmstrip') !== null;");
                    assert_filmstrip_context_contrast(control, "light");
                    click_dom(control, ".rk-top button.rk-seg__item[title='Library']", "Library segment must restore actual library view");
                    wait_for_dom(control, "return document.querySelector('.lc-library-workspace') !== null;");
                });
                with_control(&binary, scenario, &inputs.catalog, |control, _data| {
                    // Open through the actual RightKit command palette, then choose Settings by pointer.
                    click_dom(control, ".rk-search--trigger", "command palette trigger click must execute");
                    assert_eq!(wait_for_dom(control, "return document.querySelector('.rk-palette') !== null;").as_bool(), Some(true));
                    control.key("Escape").expect("command palette Escape must execute");
                    assert_eq!(wait_for_dom(control, "return document.querySelector('.rk-palette') === null;").as_bool(), Some(true));
                    // WebKit on macOS doesn't focus a button on click, so start from the trigger on every platform.
                    control.eval("document.querySelector('.rk-search--trigger')?.focus(); return true;").expect("palette trigger focus must execute");
                    let pre_modal_focus = control
                        .eval("return (() => { const node = document.activeElement; return { tag: node?.tagName, id: node?.id, className: node?.className, aria: node?.getAttribute('aria-label') }; })();")
                        .expect("pre-modal focus query must execute");
                    click_dom(control, ".rk-search--trigger", "settings command palette trigger click must execute");
                    assert_eq!(wait_for_dom(control, "return document.querySelector('.rk-palette') !== null;").as_bool(), Some(true));
                    for key in ["s", "e", "t", "t", "i", "n", "g", "s"] {
                        control.key(key).expect("command palette search key must execute");
                    }
                    assert_eq!(wait_for_dom(control, "return document.querySelector('[data-command=\"app.settings\"]') !== null;").as_bool(), Some(true));
                    click_dom(control, "[data-command=\"app.settings\"]", "settings command palette option click must execute");
                    assert_eq!(
                        wait_for_dom(control, "return document.querySelector('.lc-dialog')?.contains(document.activeElement) === true;").as_bool(),
                        Some(true),
                        "settings modal must receive focus",
                    );
                    let modal_state = control
                        .eval(
                            "return (() => { const root = document.getElementById('root'); const backdrop = document.querySelector('.lc-dialog-backdrop'); const dialog = document.querySelector('.lc-dialog'); return { dialog: Boolean(dialog), dialogInert: Boolean(dialog?.inert), backdropInert: Boolean(backdrop?.inert), rootInert: Boolean(root?.inert), rootHidden: root?.getAttribute('aria-hidden'), activeInside: Boolean(dialog?.contains(document.activeElement)) }; })();",
                        )
                        .expect("settings modal focus scope query must execute");
                    assert_eq!(modal_state["dialog"].as_bool(), Some(true));
                    assert_eq!(modal_state["dialogInert"].as_bool(), Some(false), "settings dialog must remain interactive");
                    assert_eq!(modal_state["backdropInert"].as_bool(), Some(false), "settings backdrop must remain interactive");
                    assert_eq!(modal_state["rootInert"].as_bool(), Some(true), "application background must be inert");
                    assert_eq!(modal_state["rootHidden"].as_str(), Some("true"), "application background must be hidden from assistive technology");
                    assert_eq!(modal_state["activeInside"].as_bool(), Some(true));

                    // Settings contains a real select: changing it must retain focused control through React rerender.
                    for _ in 0..24 {
                        if control.eval("return document.activeElement?.tagName === 'SELECT';").expect("focused control must be readable").as_bool() == Some(true) {
                            break;
                        }
                        control.key("Tab").expect("native Tab must reach settings select");
                    }
                    assert_eq!(control.eval("return document.activeElement?.tagName;").expect("settings select focus query must execute").as_str(), Some("SELECT"));
                    control.key("Down").expect("native select change must execute");
                    assert_eq!(wait_for_dom(control, "return document.activeElement?.tagName === 'SELECT';").as_bool(), Some(true), "focused settings select must survive rerender");

                    let focusable_count = control
                        .eval("return Array.from(document.querySelectorAll('.lc-dialog button,.lc-dialog [href],.lc-dialog input,.lc-dialog select,.lc-dialog textarea,.lc-dialog [tabindex]:not([tabindex=\"-1\"])')).filter((node) => !node.disabled).length;")
                        .expect("settings focusable count query must execute")
                        .as_u64()
                        .expect("settings modal must expose focusable controls");
                    assert!(focusable_count > 5, "settings modal must expose native tab stops");
                    let focus_index = control
                        .eval("return (() => { const dialog = document.querySelector('.lc-dialog'); const nodes = Array.from(dialog?.querySelectorAll('button,input,select,textarea,[href],[tabindex]:not([tabindex=\"-1\"])') || []).filter((node) => !node.disabled); return nodes.indexOf(document.activeElement); })();")
                        .expect("settings current focus query must execute")
                        .as_u64()
                        .expect("active settings control must be in focus cycle");
                    assert!(focus_index < focusable_count);
                    for _ in 0..((focusable_count - focus_index) % focusable_count) {
                        control.key("Tab").expect("native Tab must reset settings focus cycle");
                    }
                    for _ in 0..focusable_count {
                        control.key("Tab").expect("native Tab must execute in settings modal");
                        let inside = control
                            .eval("return document.querySelector('.lc-dialog')?.contains(document.activeElement) === true;")
                            .expect("settings focus trap query must execute");
                        assert_eq!(inside.as_bool(), Some(true), "native Tab must stay inside settings modal");
                    }
                    let wrapped = control
                        .eval("return (() => { const dialog = document.querySelector('.lc-dialog'); const nodes = Array.from(dialog?.querySelectorAll('button,input,select,textarea,[href],[tabindex]:not([tabindex=\"-1\"])') || []).filter((node) => !node.disabled); return nodes.indexOf(document.activeElement); })();")
                        .expect("settings focus wrap query must execute");
                    assert_eq!(wrapped.as_i64(), Some(0), "native Tab must wrap to first settings control");

                    control.key("Escape").expect("native Escape must close settings modal");
                    assert_eq!(wait_for_dom(control, "return document.querySelector('.lc-dialog') === null;").as_bool(), Some(true));
                    let restored = control
                        .eval("return (() => { const root = document.getElementById('root'); const node = document.activeElement; return { rootInert: Boolean(root?.inert), rootHidden: root?.getAttribute('aria-hidden'), focus: { tag: node?.tagName, id: node?.id, className: node?.className, aria: node?.getAttribute('aria-label') } }; })();")
                        .expect("settings focus restoration query must execute");
                    assert_eq!(restored["rootInert"].as_bool(), Some(false), "background inert state must be restored");
                    assert_eq!(restored["rootHidden"], Value::Null, "background aria-hidden state must be restored");
                    assert_eq!(restored["focus"], pre_modal_focus, "focus must return to actual pre-modal target");
                });
                with_control(&binary, scenario, &inputs.catalog, |control, _data| {
                    let metadata_name = "QA Settings Metadata";
                    run(control, "metadata.savePreset", json!({"name": metadata_name, "fields": {"copyright": "© QA Copyright", "creator": "QA Creator", "title": "QA Settings"}}));
                    run(control, "library.preferences", json!({"import": {"copyright": "Fixture Copyright", "creator": "Fixture Creator", "metadataPreset": metadata_name}, "cacheMb": 0}));
                    let before_library = run(control, "library.preferences", json!({}));
                    click_dom(control, ".rk-top button.rk-seg__item[title='Develop']", "Settings must preserve current Develop workspace");
                    wait_for_dom(control, "return document.querySelector('.stage-workspace') !== null;");
                    click_dom(control, ".rk-search--trigger", "settings hydration palette trigger must execute");
                    wait_for_dom(control, "return document.querySelector('.rk-palette') !== null;");
                    for key in ["s", "e", "t", "t", "i", "n", "g", "s"] {
                        control.key(key).expect("settings hydration search must execute");
                    }
                    click_dom(control, "[data-command=\"app.settings\"]", "settings hydration command must open");
                    wait_for_dom(control, "return document.querySelector('.lc-dialog h2')?.textContent === 'Settings';");
                    wait_for_dom(control, "return document.querySelector('.lc-dialog-actions .lc-button-primary')?.disabled === false;");
                    let import_tab_top = control
                        .eval("return document.querySelector('[data-settings-section=\"import\"]')?.getBoundingClientRect().top;")
                        .expect("Settings navigation geometry must be readable");
                    click_dom(control, "[data-settings-section=\"import\"]", "settings Import tab must open");
                    wait_for_dom(control, "return ['Raw photos','Default copyright','Default creator','Metadata preset'].every((label) => [...document.querySelectorAll('.lc-settings-panel .lc-field span')].some((node) => node.textContent === label)) && document.querySelector('.lc-dialog-actions .lc-button-primary')?.disabled === false;");
                    let import_tab_top_after = control
                        .eval("return document.querySelector('[data-settings-section=\"import\"]')?.getBoundingClientRect().top;")
                        .expect("Settings navigation geometry must remain readable");
                    assert_eq!(import_tab_top_after, import_tab_top, "switching Settings sections must keep navigation stationary");
                    let hydrated = control
                        .eval("return Object.fromEntries([...document.querySelectorAll('.lc-settings-panel .lc-field')].map((field) => [field.querySelector('span')?.textContent, field.querySelector('input,select')?.value]));")
                        .expect("settings hydration fields must be readable");
                    let raw_expected = before_library["import"]["rawPreset"].as_str().unwrap_or("default");
                    let other_expected = before_library["import"]["otherPreset"].as_str().unwrap_or("default");
                    let metadata_expected = before_library["import"]["metadataPreset"].as_str().unwrap_or("none");
                    assert_eq!(hydrated["Raw photos"].as_str(), Some(raw_expected), "saved raw import default must hydrate");
                    assert_eq!(hydrated["Non-raw photos"].as_str(), Some(other_expected), "saved non-raw import default must hydrate");
                    assert_eq!(hydrated["Metadata preset"].as_str(), Some(metadata_expected), "saved metadata preset must hydrate");
                    assert_eq!(hydrated["Default copyright"].as_str(), Some("Fixture Copyright"), "saved copyright default must hydrate");
                    assert_eq!(hydrated["Default creator"].as_str(), Some("Fixture Creator"), "saved creator default must hydrate");
                    click_dom(control, "[data-settings-section=\"performance\"]", "settings Performance tab must open");
                    wait_for_dom(control, "return [...document.querySelectorAll('.lc-settings-panel .lc-field span')].some((node) => node.textContent === 'Memory budget (MB; 0 = automatic)');");
                    let memory = control
                        .eval("return [...document.querySelectorAll('.lc-settings-panel .lc-field')].find((field) => field.querySelector('span')?.textContent === 'Memory budget (MB; 0 = automatic)')?.querySelector('input')?.value || ''; ")
                        .expect("automatic memory setting must be readable");
                    assert_eq!(memory.as_str(), Some("0"), "fresh settings must expose automatic memory budget");
                    click_dom(control, "[data-settings-section=\"general\"]", "settings General tab must open");
                    set_dialog_field(control, "Theme", "dark");
                    // Simulate a persisted workspace that lags the live UI debounce.
                    control.command("lc_preferences", &json!({"ui": {"view": "photoGrid"}})).expect("stale persisted workspace fixture must save");
                    click_dom(control, ".lc-dialog-actions .lc-button-primary", "settings field-only save must execute");
                    assert_eq!(wait_for_dom(control, "return document.querySelector('.lc-dialog') === null;").as_bool(), Some(true));
                    wait_for_dom(control, "return document.querySelector('.stage-workspace')?.classList.contains('stage-dark') === true;");
                    let after_library = run(control, "library.preferences", json!({}));
                    assert_eq!(after_library["import"], before_library["import"], "theme-only Settings save must preserve import defaults & metadata preset");
                    let persisted = control.command("lc_preferences", &Value::Null).expect("theme preference must reopen after field-only save");
                    assert_eq!(persisted["ui"]["theme"].as_str(), Some("dark"));
                    click_dom(control, ".rk-search--trigger", "settings parity palette must open");
                    wait_for_dom(control, "return document.querySelector('.rk-palette') !== null;");
                    for key in ["s", "e", "t", "t", "i", "n", "g", "s"] {
                        control.key(key).expect("settings parity search must execute");
                    }
                    click_dom(control, "[data-command=\"app.settings\"]", "settings parity dialog must open");
                    wait_for_dom(control, "return document.querySelector('.lc-dialog-actions .lc-button-primary')?.disabled === false;");
                    set_dialog_field(control, "Open in", "detail");
                    set_dialog_field(control, "Application", "QA External Editor");
                    click_dom(control, "[data-settings-section=\"interface\"]", "Interface settings must open");
                    set_dialog_field(control, "Info overlay", "2");
                    click_dom(control, ".lc-dialog-actions .lc-button-primary", "settings parity values must save");
                    wait_for_dom(control, "return document.querySelector('.lc-dialog') === null;");
                    let parity = control.command("lc_preferences", &Value::Null).expect("new settings must persist");
                    assert_eq!(parity["ui"]["startupView"].as_str(), Some("detail"));
                    assert_eq!(parity["ui"]["externalEditor"].as_str(), Some("QA External Editor"));
                    assert_eq!(parity["ui"]["infoOverlay"].as_u64(), Some(2));
                    assert_eq!(parity["qaSentinel"].as_str(), Some("preserved"));
                    control.command("lc_preferences", &json!({"ui": {"theme": "light"}})).expect("settings journey must restore light theme");
                });
                with_control(&binary, scenario, &inputs.catalog, |control, _data| {
                    if snapshot(control)["active"].is_null() {
                        task_result(control, "library.import", json!({"paths": [inputs.png], "mode": "add"}));
                    }
                    wait_for_dom(control, "return document.querySelector('.lc-stage-layout') !== null && document.querySelector('.info-overlay') !== null;");
                    let exposure_overlay = control.eval("return document.querySelector('.info-overlay')?.textContent || '';").expect("Exposure overlay must render");
                    click_dom(control, ".footer-right button:first-child", "Info control must cycle Exposure to Off");
                    wait_for_dom(control, "return document.querySelector('.info-overlay') === null;");
                    click_dom(control, ".footer-right button:first-child", "Info control must cycle Off to Basic");
                    wait_for_dom(control, "return document.querySelector('.info-overlay') !== null;");
                    let basic_overlay = control.eval("return document.querySelector('.info-overlay')?.textContent || '';").expect("Basic overlay must render");
                    assert_ne!(basic_overlay, exposure_overlay, "Basic & Exposure modes must show different authoritative photo metadata");
                    click_dom(control, ".rk-search--trigger", "shortcut help palette must open");
                    wait_for_dom(control, "return document.querySelector('.rk-palette') !== null;");
                    for key in ["s", "h", "o", "r", "t", "c", "u", "t", "s"] {
                        control.key(key).expect("shortcut help search must execute");
                    }
                    click_dom(control, "[data-command=\"app.shortcuts\"]", "shortcut help must open through app route");
                    wait_for_dom(control, "return document.querySelectorAll('.lc-shortcuts kbd').length > 20;");
                    control.key("Escape").expect("shortcut help must close");
                });
            } else if name == "mergeHdr" {
                with_control(&binary, scenario, &inputs.catalog, |control, _data| {
                    let paths = merge_inputs.iter().map(|path| path.to_string_lossy().to_string()).collect::<Vec<_>>();
                    let review = import_review(control, json!({"paths": paths}));
                    let candidates = review["candidates"].as_array().expect("HDR merge fixture review must return candidates");
                    assert_eq!(candidates.len(), 2, "HDR merge review must expose two procedural PNGs: {review}");
                    assert!(candidates.iter().all(|candidate| candidate["duplicate"].is_null()), "HDR merge fixtures must be distinct import candidates: {review}");
                    let started = run(control, "library.import", json!({"paths": paths, "mode": "add"}));
                    let import_task = started["taskId"].as_str().expect("HDR fixture import must return task id").to_string();
                    let imported_snapshot = wait_task(control, &import_task);
                    let imported_receipt = completed_task(&imported_snapshot, &import_task);
                    assert_eq!(imported_receipt["state"].as_str(), Some("done"), "HDR fixture import must complete: {imported_receipt}");
                    let ids = imported_receipt["result"]["imported"]
                        .as_array()
                        .expect("HDR fixture import receipt must list imported ids")
                        .iter()
                        .map(|id| id.as_u64().expect("HDR fixture photo id must be numeric"))
                        .collect::<Vec<_>>();
                    assert_eq!(ids.len(), 2, "HDR fixture import must add both procedural PNGs: {imported_receipt}");
                    run(control, "library.select", json!({"ids": ids, "active": ids[0], "mode": "replace"}));

                    click_dom(control, ".rk-search--trigger", "merge command palette trigger must execute");
                    wait_for_dom(control, "return document.querySelector('.rk-palette') !== null;");
                    for key in ["h", "d", "r"] {
                        control.key(key).expect("HDR command palette search must execute");
                    }
                    wait_for_dom(control, "return document.querySelector('[data-command=\"dialog.mergeHdr\"]') !== null;");
                    click_dom(control, "[data-command=\"dialog.mergeHdr\"]", "HDR merge command must open React dialog");
                    wait_for_dom(control, "return document.querySelector('.lc-dialog h2')?.textContent === 'HDR Merge Preview';");
                    let first_preview = wait_for_rendered_preview(control, ".lc-merge-preview img", None);
                    let first_src = first_preview["src"].as_str().expect("HDR preview must expose scoped source handle").to_string();
                    assert!(first_preview["naturalWidth"].as_u64().is_some_and(|width| width > 0));
                    assert!(first_preview["naturalHeight"].as_u64().is_some_and(|height| height > 0));
                    let summary = control
                        .eval("return document.querySelector('.lc-merge-preview-info')?.textContent?.trim() || '';" )
                        .expect("HDR preview summary query must execute");
                    assert!(summary.as_str().is_some_and(|text| text.contains("2 photos")), "HDR preview must summarize both selected photos: {summary}");
                    let first_screenshot = scenario.dir().join("merge-hdr-preview-initial.png");
                    control.screenshot_to(&first_screenshot).expect("initial HDR preview screenshot must save");

                    set_dialog_field(control, "Deghost amount", "medium");
                    let second_preview = wait_for_rendered_preview(control, ".lc-merge-preview img", Some(&first_src));
                    let second_src = second_preview["src"].as_str().expect("updated HDR preview must expose scoped source handle");
                    assert_ne!(second_src, first_src, "HDR option update must publish a fresh preview handle");
                    assert!(second_preview["naturalWidth"].as_u64().is_some_and(|width| width > 0));
                    assert!(second_preview["naturalHeight"].as_u64().is_some_and(|height| height > 0));
                    let second_screenshot = scenario.dir().join("merge-hdr-preview-deghost.png");
                    control.screenshot_to(&second_screenshot).expect("updated HDR preview screenshot must save");

                    click_dom(control, ".lc-dialog-actions .lc-button-primary", "HDR merge submit must execute");
                    wait_for_dom(control, "return document.querySelector('.lc-dialog h2')?.textContent === 'Merging Photos';");
                    let mut merge_task = None;
                    for _ in 0..200 {
                        let current = snapshot(control);
                        merge_task = current["status"]["jobs"]
                            .as_array()
                            .and_then(|jobs| jobs.iter().find(|job| job["kind"].as_str() == Some("merge")))
                            .and_then(|job| job["id"].as_str())
                            .map(str::to_string)
                            .or_else(|| current["status"]["completedJobs"].as_array().and_then(|jobs| jobs.iter().find(|job| job["kind"].as_str() == Some("merge"))).and_then(|job| job["id"].as_str()).map(str::to_string));
                        if merge_task.is_some() { break; }
                        sleep(Duration::from_millis(50));
                    }
                    let merge_task = merge_task.expect("React HDR submit must expose merge task id through native snapshot");
                    let finished = wait_task(control, &merge_task);
                    let receipt = completed_task(&finished, &merge_task);
                    assert_eq!(receipt["state"].as_str(), Some("done"), "HDR merge task must complete: {receipt}");
                    let result = &receipt["result"];
                    let output_path = result["path"].as_str().expect("HDR merge receipt must expose output path");
                    assert!(output_path.ends_with(".dng"), "HDR merge output must be DNG: {result}");
                    let output = PathBuf::from(output_path);
                    let bytes = fs::read(&output).expect("HDR merge DNG output must be readable");
                    assert!(bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*"), "HDR merge output must contain TIFF/DNG bytes");
                    assert!(bytes.len() > 1024, "HDR merge DNG output must contain image data");
                    assert!(finished["counts"]["catalog"].as_u64().is_some_and(|count| count >= 3), "HDR merge must import merged output into catalog: {finished}");
                    let merged_id = result["id"].as_u64().expect("HDR merge receipt must expose merged photo id");
                    assert_eq!(finished["active"].as_u64(), Some(merged_id), "completed HDR merge must select merged photo");
                    let merged_generation = finished["viewGeneration"].as_u64().expect("merged photo view generation must be numeric");
                    let decoded = control
                        .command("lc_preview", &json!({"request": {"photoId": merged_id, "slot": "merge-qualification", "viewGeneration": merged_generation, "width": 96, "height": 64, "quality": "draft", "before": false, "sequence": 1}}))
                        .expect("merged DNG must decode through lc_preview");
                    assert!(decoded["handle"].as_str().is_some_and(|handle| !handle.is_empty()), "merged DNG preview must return scoped handle: {decoded}");
                    assert!(decoded["width"].as_u64().is_some_and(|width| width > 0));
                    assert!(decoded["height"].as_u64().is_some_and(|height| height > 0));
                    let decoded_handle = decoded["handle"].as_str().expect("merged preview handle must be text").to_string();
                    control.command("lc_preview_ack", &json!({"handle": decoded_handle})).expect("merged preview handle must be acknowledged");
                    wait_for_dom(control, "return [...document.querySelectorAll('.lc-dialog-actions button')].some((button) => button.textContent === 'Close' && !button.disabled);");
                    click_dom(control, ".lc-dialog-actions .lc-button", "completed HDR dialog must close");
                    wait_for_dom(control, "return document.querySelector('.lc-dialog') === null;");
                    control.key("D").expect("merged DNG develop route must execute");
                    wait_for_dom(control, "return document.querySelector('.stage-workspace.stage-detail') !== null;");
                    let merged_dom = wait_for_rendered_preview(control, "img.stage-preview", None);
                    assert!(merged_dom["naturalWidth"].as_u64().is_some_and(|width| width > 0), "merged DNG WebView preview must decode");
                    assert!(merged_dom["naturalHeight"].as_u64().is_some_and(|height| height > 0), "merged DNG WebView preview must decode");
                    let receipt_path = scenario.dir().join("merge-hdr.json");
                    fs::write(&receipt_path, serde_json::to_vec_pretty(&json!({
                        "schema": 1,
                        "journey": "react-hdr-preview-option-update-completed-job",
                        "sourcePaths": paths,
                        "importTask": import_task,
                        "importedIds": ids,
                        "initialPreview": first_preview,
                        "updatedPreview": second_preview,
                        "mergeTask": merge_task,
                        "completedJob": receipt,
                        "outputDng": output,
                        "decodedMergedPreview": decoded,
                        "renderedMergedPreview": merged_dom,
                    })).expect("HDR merge receipt must serialize")).expect("HDR merge receipt must write");
                    scenario.keep("merge-hdr-preview-initial.png", &first_screenshot);
                    scenario.keep("merge-hdr-preview-deghost.png", &second_screenshot);
                    scenario.keep("merge-hdr.json", &receipt_path);
                });
            } else if name == "catalogRecovery" {
                run_catalog_recovery(&binary, scenario, &inputs.catalog, &inputs.png, &baseline_capture, &revision, &installed_hash);
            } else {
                with_control(&binary, scenario, &inputs.catalog, |control, _data| match name {
                    "ipc" => {
                        let health = control.health().expect("health RPC must reply");
                        assert!(health.is_object(), "health RPC must return object");
                        let snapshot = control.command("lc_snapshot", &Value::Null).expect("snapshot command must reply");
                        assert_eq!(snapshot["version"].as_u64(), Some(1));
                        assert!(snapshot["viewGeneration"].is_number());
                        assert!(snapshot["controls"].is_array());
                        import_file(control, &inputs.png);
                        control.key("G").expect("populated library route must execute");
                        wait_for_rendered_preview(control, "img.lc-photo-preview", None);
                        wait_for_rendered_preview(control, "img.lc-filmstrip-preview", None);
                        for (width, height) in [(1280_u64, 800_u64), (1600_u64, 1000_u64)] {
                            set_native_viewport(control, width, height);
                            wait_for_dom(control, "return document.querySelector('.lc-library-workspace') !== null;");
                            assert_layout_settled(control, ".lc-library-workspace");
                            assert_workspace_design(control);
                            let viewport = control
                                .eval("return {width: window.innerWidth, height: window.innerHeight};")
                                .expect("viewport query must execute");
                            assert_eq!(viewport["width"].as_u64(), Some(width));
                            assert_eq!(viewport["height"].as_u64(), Some(height));
                            let library = scenario.dir().join(format!("route-library-{width}x{height}.png"));
                            control.screenshot_to(&library).expect("library route screenshot must be captured");
                            assert!(library.is_file());
                            control.key("D").expect("develop route key must execute");
                            wait_for_dom(control, "return document.querySelector('.stage-workspace.stage-detail') !== null;");
                            assert_layout_settled(control, ".stage-workspace.stage-detail");
                            choose_inspector_panel(control, "edit");
                            wait_for_dom(control, "return document.querySelector('input[id=\"ctl-light.exposure\"]') !== null;");
                            assert_primary_editing_usable(control);
                            assert_inspector_panels_fit(control);
                            click_dom(control, ".lc-inspector__section-head", "Light section must collapse by pointer");
                            wait_for_dom(control, "return document.querySelector('input[id=\"ctl-light.exposure\"]') === null;");
                            control.key(if cfg!(target_os = "macos") { "Cmd+1" } else { "Ctrl+1" }).expect("Light shortcut must reopen canonical section state");
                            wait_for_dom(control, "return document.querySelector('input[id=\"ctl-light.exposure\"]') !== null;");
                            assert_primary_editing_usable(control);
                            assert_workspace_design(control);
                            let route = control
                                .eval("return document.querySelector('.stage-view-mode select')?.value || '';")
                                .expect("develop route readback must execute");
                            assert_eq!(route.as_str(), Some("detail"), "D must select Detail route");
                            let develop = scenario.dir().join(format!("route-develop-{width}x{height}.png"));
                            wait_for_rendered_preview(control, "img.stage-preview", None);
                            capture_visible_tool_preview(control, &develop);
                            assert!(develop.is_file());
                            let identity_nonce = RETAINED_LAYOUT_IDENTITY_NONCE.fetch_add(1, Ordering::Relaxed);
                            let identity_key = format!("__lcRetainedLayoutIdentity_{identity_nonce}");
                            let identity_owner = format!("retained-layout:{width}x{height}:{identity_nonce}");
                            let identity_setup = control
                                .eval(&format!("return (() => {{ const key = {identity_key:?}; const owner = {identity_owner:?}; if (Object.prototype.hasOwnProperty.call(window, key)) return {{owned:false, owner, error:'probe state collision'}}; const stage = document.querySelector('.stage-workspace'); const inspector = document.querySelector('.lc-inspector'); const layout = inspector?.closest('.lc-stage-layout,.lc-library-layout'); const collapse = inspector?.querySelector('button[data-inspector-collapse]'); const preview = document.querySelector('img.stage-preview'); const previewFrame = preview?.closest('.photo-preview-frame'); if (!stage || !inspector || !layout || !collapse || !preview || !previewFrame || !preview.complete || preview.naturalWidth < 1 || preview.naturalHeight < 1) return {{owned:false, owner, error:'retained nodes, collapse button or ready preview frame missing'}}; collapse.focus(); if (document.activeElement !== collapse) return {{owned:false, owner, error:'collapse button could not receive focus'}}; window[key] = {{owner, layout, stage, inspector, collapse, preview, previewFrame}}; return {{owned:true, owner, focused:true}}; }})();"))
                                .expect("retained layout identity probe must execute");
                            assert_eq!(identity_setup["owned"].as_bool(), Some(true), "retained layout identity probe must initialize: {identity_setup}");
                            click_dom(control, ".lc-inspector button[data-inspector-collapse]", "manual inspector collapse must execute");
                            wait_for_retained_layout_collapse(control, &identity_key, &identity_owner);
                            click_dom(control, ".lc-inspector button[data-more-tools]", "collapsed More tools must open");
                            let collapsed_menu = control.eval("return (() => { const e = document.querySelector('.lc-inspector__more-menu'); if (!e) return null; const r = e.getBoundingClientRect(); const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); const inspector = document.querySelector('.lc-inspector'); const layout = inspector?.closest('.lc-stage-layout,.lc-library-layout'); return { width: inspector?.getBoundingClientRect().width, layout: layout?.className, columns: layout ? getComputedStyle(layout).gridTemplateColumns : null, inside: r.left >= 0 && r.right <= innerWidth && r.top >= 0 && r.bottom <= innerHeight, reachable: e.contains(hit) }; })();").expect("collapsed More geometry must be queryable");
                            assert!(collapsed_menu["width"].as_f64().is_some_and(|value| (value - 44.0).abs() < 2.0), "collapsed inspector must retain44px rail: {collapsed_menu}");
                            assert_eq!(collapsed_menu["inside"].as_bool(), Some(true), "collapsed menu must remain inside viewport: {collapsed_menu}");
                            assert_eq!(collapsed_menu["reachable"].as_bool(), Some(true), "collapsed More menu must be visible & reachable: {collapsed_menu}");
                            control.key("Escape").expect("More dismissal must execute");
                            click_dom(control, ".lc-inspector button[data-inspector-collapse]", "manual inspector expansion must execute");
                            wait_for_dom(control, "return document.querySelector('.lc-inspector')?.classList.contains('is-collapsed') === false;");
                            let identity_cleanup = control
                                .eval(&format!("return (() => {{ const key = {identity_key:?}; const owner = {identity_owner:?}; const state = window[key]; if (!state) return {{owned:false, owner, error:'probe state missing'}}; if (state.owner !== owner) return {{owned:false, owner, error:'probe state ownership mismatch'}}; delete window[key]; return {{owned:true, owner, deleted:!Object.prototype.hasOwnProperty.call(window, key)}}; }})();"))
                                .expect("retained layout identity probe cleanup must execute");
                            assert_eq!(identity_cleanup["owned"].as_bool(), Some(true), "retained layout identity probe cleanup must be owned: {identity_cleanup}");
                            assert_eq!(identity_cleanup["deleted"].as_bool(), Some(true), "retained layout identity probe cleanup must delete state: {identity_cleanup}");
                            assert_workspace_design(control);
                            control.key("G").expect("library route key must execute");
                            wait_for_dom(control, "return document.querySelector('.lc-library-workspace') !== null;");
                        }
                    }
                    "stalePreview" => {
                        let first = import_file(control, &inputs.png);
                        control.key("G").expect("library route key must execute");
                        wait_for_dom(control, "return document.querySelector('.lc-library-workspace') !== null;");
                        let first_grid = wait_for_active_grid_preview(control, "procedural-rgb-01.png", None);
                        assert_active_grid_identity(control, "procedural-rgb-01.png");
                        let first_src = first_grid["src"].as_str().expect("first grid preview must expose scoped source").to_string();
                        let first_id = first["active"].as_u64().expect("first import must select active photo");
                        let second = import_file(control, &inputs.arw);
                        let second_generation = second["viewGeneration"].as_u64().expect("second import generation required");
                        let second_grid = wait_for_active_grid_preview(control, "synthetic-sonya-01.arw", Some(&first_src));
                        assert_active_grid_identity(control, "synthetic-sonya-01.arw");
                        let second_src = second_grid["src"].as_str().expect("second grid preview must expose scoped source").to_string();
                        assert_ne!(first_src, second_src, "second import must replace active grid pixels");
                        let second_id = second["active"].as_u64().expect("second import must select active photo");
                        let _selected_first = run(control, "library.select", json!({"ids": [first_id], "active": first_id, "mode": "replace"}));
                        let first_again = snapshot(control);
                        assert_eq!(first_again["active"].as_u64(), Some(first_id), "selection snapshot must expose first active photo");
                        assert_eq!(first_again["viewGeneration"].as_u64(), Some(second_generation), "selection must preserve visible view generation");
                        let first_again_grid = wait_for_active_grid_preview(control, "procedural-rgb-01.png", Some(&second_src));
                        assert_active_grid_identity(control, "procedural-rgb-01.png");
                        assert_ne!(first_again_grid["src"].as_str(), Some(second_src.as_str()), "quick selection must replace second photo pixels");
                        control.key("D").expect("develop route key must execute");
                        wait_for_dom(control, "return document.querySelector('.stage-workspace.stage-detail') !== null;");
                        let first_stage = wait_for_rendered_preview(control, "img.stage-preview", None);
                        let first_stage_src = first_stage["src"].as_str().expect("first stage preview must expose scoped source").to_string();
                        let _selected_second = run(control, "library.select", json!({"ids": [second_id], "active": second_id, "mode": "replace"}));
                        let second_again = snapshot(control);
                        assert_eq!(second_again["active"].as_u64(), Some(second_id), "selection snapshot must expose second active photo");
                        assert_eq!(second_again["viewGeneration"], first_again["viewGeneration"], "selection must preserve visible view generation");
                        let second_stage = wait_for_rendered_preview(control, "img.stage-preview", Some(&first_stage_src));
                        assert_ne!(second_stage["src"].as_str(), Some(first_stage_src.as_str()), "quick selection must never leave first photo in stage");
                        control.key("G").expect("library route key must execute");
                        wait_for_dom(control, "return document.querySelector('.lc-library-workspace') !== null;");
                        wait_for_active_grid_preview(control, "synthetic-sonya-01.arw", None);
                        assert_active_grid_identity(control, "synthetic-sonya-01.arw");
                        let stale = control
                            .command("lc_view_slice", &json!({"generation": second_again["viewGeneration"].as_u64().unwrap_or(0) + 1, "offset": 0, "limit": 512}))
                            .expect("slice must reply");
                        assert_eq!(stale["generationChanged"].as_bool(), Some(true), "stale generation must be rejected");
                    }
                    "cullReview" => {
                        let opened = control
                            .command("lc_native", &json!({"action": "openLibrary", "params": {"path": inputs.scalability_library}}))
                            .expect("cull review must open procedural demo library");
                        let library_path = inputs.scalability_library.to_string_lossy().to_string();
                        assert_eq!(opened["path"].as_str(), Some(library_path.as_str()));
                        let opened_snapshot = snapshot(control);
                        let generation = opened_snapshot["viewGeneration"].as_u64().expect("cull review generation must be numeric");
                        let page = control
                            .command("lc_view_slice", &json!({"generation": generation, "offset": 0, "limit": 4}))
                            .expect("cull review demo slice must reply");
                        let ids = page["photos"]
                            .as_array()
                            .expect("cull review demo slice must expose photos")
                            .iter()
                            .take(4)
                            .map(|photo| photo["id"].as_u64().expect("cull review photo id must be numeric"))
                            .collect::<Vec<_>>();
                        assert!(ids.len() >= 2, "cull review must have multiple procedural photos");
                        run(control, "library.select", json!({"ids": ids, "active": ids[0], "mode": "replace"}));
                        let baseline = snapshot(control);
                        let baseline_slice = control
                            .command("lc_view_slice", &json!({"generation": baseline["viewGeneration"], "offset": 0, "limit": 4}))
                            .expect("cull review baseline slice must reply");
                        let expected_selected_names = baseline_slice["photos"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|photo| photo["id"].as_u64().is_some_and(|id| ids.contains(&id)))
                            .filter_map(|photo| photo["fileName"].as_str())
                            .collect::<Vec<_>>();
                        assert_eq!(expected_selected_names.len(), ids.len(), "cull review readiness must identify every selected photo");
                        let expected_selected_names_json = serde_json::to_string(&expected_selected_names)
                            .expect("cull review readiness names must serialize");
                        wait_for_dom(
                            control,
                            &format!(
                                "return (() => {{ const title = document.querySelector('.lc-library-title')?.textContent || ''; const selected = [...document.querySelectorAll('.lc-photo-cell[aria-selected=\\\"true\\\"] .lc-photo-name')].map((node) => node.textContent?.trim() || '').sort(); const expected = {expected_selected_names_json}; return title.includes('{count} selected') && JSON.stringify(selected) === JSON.stringify([...expected].sort()); }})();",
                                count = ids.len(),
                            ),
                        );

                        click_dom(control, ".rk-search--trigger", "cull review command palette must open");
                        wait_for_dom(control, "return document.querySelector('.rk-palette') !== null;");
                        for key in ["c", "u", "l", "l"] {
                            control.key(key).expect("cull review palette search must execute");
                        }
                        click_dom(control, "[data-command=\"dialog.cull\"]", "cull review dialog command must open");
                        wait_for_dom(control, "return document.querySelector('.lc-cull-review-dialog') !== null;");
                        wait_for_dom(control, "return document.querySelector('.lc-cull-review-progress') === null && document.querySelectorAll('.lc-cull-review-row').length > 0;");
                        let initial_apply_disabled = control
                            .eval("return document.querySelector('.lc-cull-review-button-primary')?.disabled === true;")
                            .expect("initial cull review apply state must be queryable");
                        assert_eq!(initial_apply_disabled.as_bool(), Some(true), "cull review must keep Apply disabled before explicit selection");

                        // Default pick-best analysis can legitimately produce no suggestions for distinct demo frames.
                        // Re-run with an explicit reject threshold so this journey exercises one accepted suggestion.
                        click_dom(control, ".lc-cull-review-settings input[type=\"checkbox\"]", "cull review reject suggestions must enable");
                        let threshold = control
                            .eval("return (() => { const node = document.querySelector('.lc-cull-review-threshold input'); if (!node) return false; Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(node, '100'); node.dispatchEvent(new Event('input', {bubbles:true})); return node.value === '100'; })();")
                            .expect("cull review threshold must accept explicit value");
                        assert_eq!(threshold.as_bool(), Some(true));
                        click_dom(control, ".lc-cull-review-settings .lc-cull-review-button-secondary", "cull review suggestions must regenerate");
                        wait_for_dom(control, "return document.querySelector('.lc-cull-review-progress') === null && [...document.querySelectorAll('.lc-cull-review-row .lc-cull-review-check input')].some((node) => !node.disabled && node.parentElement?.textContent?.includes('Accept'));");

                        let analyzed = snapshot(control);
                        for key in ["counts", "active", "selection", "viewGeneration", "source", "filter", "sort", "undo"] {
                            assert_eq!(analyzed[key], baseline[key], "read-only cull analysis must preserve snapshot field {key}");
                        }
                        let analyzed_slice = control
                            .command("lc_view_slice", &json!({"generation": analyzed["viewGeneration"], "offset": 0, "limit": 4}))
                            .expect("cull review analyzed slice must reply");
                        assert_eq!(analyzed_slice["photos"], baseline_slice["photos"], "read-only cull analysis must preserve catalog flags");
                        let review_screenshot = scenario.dir().join("cull-review-unselected.png");
                        control.screenshot_to(&review_screenshot).expect("cull review empty-accept screenshot must save");
                        assert!(review_screenshot.is_file());
                        scenario.keep("cull-review-unselected.png", &review_screenshot);

                        let suggested = control
                            .eval("return (() => { const rows = [...document.querySelectorAll('.lc-cull-review-row')]; const index = rows.findIndex((row) => { const input = row.querySelector('.lc-cull-review-check input'); return input && !input.disabled && /Accept (pick|reject)/.test(row.textContent || ''); }); if (index < 0) return null; const row = rows[index]; const id = Number((row.textContent || '').match(/Photo ID (\\d+)/)?.[1]); const flag = (row.textContent || '').match(/Accept (pick|reject)/)?.[1] || ''; return { index: index + 1, id, flag }; })();")
                            .expect("cull review suggested row must be queryable");
                        assert!(suggested.is_object(), "cull review must expose explicit suggested checkbox");
                        let row_index = suggested["index"].as_u64().expect("cull review row index must be numeric");
                        let suggested_id = suggested["id"].as_u64().expect("cull review suggested id must be numeric");
                        let suggested_flag = suggested["flag"].as_str().expect("cull review suggested flag must be text");
                        click_dom(
                            control,
                            &format!(".lc-cull-review-row:nth-child({row_index}) .lc-cull-review-check input"),
                            "cull review explicit suggestion checkbox must select",
                        );
                        wait_for_dom(control, "return document.querySelector('.lc-cull-review-button-primary')?.disabled === false;");
                        click_dom(control, ".lc-cull-review-button-primary", "cull review apply selected must execute");
                        wait_for_dom(control, "return document.querySelector('.lc-cull-review-dialog') === null;");
                        let applied = snapshot(control);
                        assert!(applied["undo"].as_u64().is_some_and(|count| count > baseline["undo"].as_u64().unwrap_or(0)), "cull apply must create one undo step");
                        let applied_slice = control
                            .command("lc_view_slice", &json!({"generation": applied["viewGeneration"], "offset": 0, "limit": 4}))
                            .expect("cull review applied slice must reply");
                        let applied_photo = applied_slice["photos"]
                            .as_array()
                            .and_then(|photos| photos.iter().find(|photo| photo["id"].as_u64() == Some(suggested_id)))
                            .expect("cull review applied photo must remain visible");
                        assert_eq!(applied_photo["flag"].as_str(), Some(suggested_flag), "explicit cull accept must set suggested flag");

                        run(control, "edit.undo", json!({}));
                        let restored = wait_for_snapshot(control, |value| value["undo"] == baseline["undo"], "cull review undo must restore baseline history");
                        let restored_slice = control
                            .command("lc_view_slice", &json!({"generation": restored["viewGeneration"], "offset": 0, "limit": 4}))
                            .expect("cull review restored slice must reply");
                        assert_eq!(restored["counts"], baseline["counts"], "cull review undo must preserve catalog counts");
                        assert_eq!(restored["selection"], baseline["selection"], "cull review undo must preserve selection");
                        assert_eq!(restored_slice["photos"], baseline_slice["photos"], "cull review undo must restore flags");

                        click_dom(control, ".rk-search--trigger", "cull review cancel palette must open");
                        wait_for_dom(control, "return document.querySelector('.rk-palette') !== null;");
                        for key in ["c", "u", "l", "l"] {
                            control.key(key).expect("cull review cancel palette search must execute");
                        }
                        click_dom(control, "[data-command=\"dialog.cull\"]", "cull review cancel dialog must open");
                        wait_for_dom(control, "return document.querySelector('.lc-cull-review-dialog') !== null;");
                        let expected_review_rows = ids.len();
                        wait_for_dom(
                            control,
                            &format!(
                                "return document.querySelector('.lc-cull-review-progress') === null && document.querySelectorAll('.lc-cull-review-row').length === {expected_review_rows};"
                            ),
                        );
                        click_dom(control, ".lc-cull-review-footer .lc-cull-review-button-secondary", "cull review Cancel must close without mutation");
                        wait_for_dom(control, "return document.querySelector('.lc-cull-review-dialog') === null;");
                        let cancelled = snapshot(control);
                        let cancelled_slice = control
                            .command("lc_view_slice", &json!({"generation": cancelled["viewGeneration"], "offset": 0, "limit": 4}))
                            .expect("cull review cancelled slice must reply");
                        assert_eq!(cancelled["counts"], baseline["counts"], "cull review cancel must preserve catalog counts");
                        assert_eq!(cancelled["selection"], baseline["selection"], "cull review cancel must preserve selection");
                        assert_eq!(cancelled["undo"], baseline["undo"], "cull review cancel must preserve undo history");
                        assert_eq!(cancelled_slice["photos"], baseline_slice["photos"], "cull review cancel must preserve flags");
                    }
                    "cache" => {
                        let imported = import_file(control, &inputs.png);
                        control.key("G").expect("library route key must execute");
                        wait_for_dom(control, "return document.querySelector('.lc-library-workspace') !== null;");
                        wait_for_rendered_preview(control, ".lc-photo-cell.is-active img.lc-photo-preview", None);
                        assert_active_grid_identity(control, "procedural-rgb-01.png");
                        let slice = control
                            .command("lc_view_slice", &json!({"generation": imported["viewGeneration"], "offset": 0, "limit": 4096}))
                            .expect("bounded slice must reply");
                        assert!(slice["photos"].as_array().is_some_and(|photos| photos.len() <= 512));
                        assert_eq!(slice["generation"], imported["viewGeneration"]);
                        let dom = control.dom(".lc-grid-scroll").expect("grid DOM query must execute");
                        assert!(!dom.is_empty(), "library grid must exist in hidden WebView");
                        assert_active_grid_is_bounded(control);
                    }
                    "scalability" => {
                        let opened = control
                            .command("lc_native", &json!({"action": "openLibrary", "params": {"path": inputs.scalability_library}}))
                            .expect("native scalability openLibrary command must execute");
                        let library_path = inputs.scalability_library.to_string_lossy().to_string();
                        assert_eq!(opened["path"].as_str(), Some(library_path.as_str()), "scalability openLibrary must report generated path");
                        let opened_snapshot = snapshot(control);
                        assert_eq!(opened_snapshot["libraryPath"].as_str(), Some(library_path.as_str()), "snapshot must expose scalability library path");
                        assert_eq!(opened_snapshot["counts"]["catalog"].as_u64(), Some(fixture_inputs::SCALABILITY_PHOTO_COUNT as u64));
                        assert_eq!(opened_snapshot["total"].as_u64(), Some(fixture_inputs::SCALABILITY_PHOTO_COUNT as u64));
                        let generation = opened_snapshot["viewGeneration"].as_u64().expect("scalability generation must be numeric");
                        let first_page = control
                            .command("lc_view_slice", &json!({"generation": generation, "offset": 0, "limit": 4096}))
                            .expect("scalability first page must reply");
                        assert_eq!(first_page["total"].as_u64(), Some(fixture_inputs::SCALABILITY_PHOTO_COUNT as u64));
                        assert_eq!(first_page["offset"].as_u64(), Some(0));
                        assert_eq!(first_page["photos"].as_array().map(Vec::len), Some(512), "first page must remain capped at 512 photos");
                        let tail_page = control
                            .command("lc_view_slice", &json!({"generation": generation, "offset": 1536, "limit": 512}))
                            .expect("scalability tail page must reply");
                        assert_eq!(tail_page["offset"].as_u64(), Some(1536));
                        assert_eq!(tail_page["photos"].as_array().map(Vec::len), Some(512), "tail page must contain final 512 photos");
                        let first_id = first_page["photos"][0]["id"].as_u64().expect("first page must expose photo id");
                        let _selected = run(control, "library.select", json!({"ids": [first_id], "active": first_id, "mode": "replace"}));
                        let selected_snapshot = snapshot(control);
                        assert_eq!(selected_snapshot["active"].as_u64(), Some(first_id), "scalability selection snapshot must identify first page photo");
                        control.key("G").expect("library route key must execute for scalability journey");
                        wait_for_dom(control, "return document.querySelector('.lc-library-workspace') !== null;");
                        assert_layout_settled(control, ".lc-grid-scroll");
                        enable_grid_info(control);
                        let initial = wait_for_grid(control, false, None);
                        let initial_name = initial["first"].as_str().unwrap_or_default().to_string();
                        assert!(!initial_name.is_empty(), "initial grid must expose first filename");
                        assert!(initial["scrollHeight"].as_f64().unwrap_or(0.0) > initial["clientHeight"].as_f64().unwrap_or(0.0), "2048-photo grid must have scrollable height: {initial}");
                        assert_active_grid_is_bounded(control);
                        assert_grid_geometry_retains_focus_and_preview(control);
                        scroll_grid_to_end(control);
                        let far = wait_for_grid(control, true, Some(initial_name.as_str()));
                        assert!(far["scrollHeight"].as_f64().unwrap_or(0.0) > far["clientHeight"].as_f64().unwrap_or(0.0), "far grid viewport must preserve scrollable height: {far}");
                        assert!(far["cells"].as_u64().is_some_and(|count| count <= 512), "far grid rendered too many cells: {far}");
                        assert!(far["images"].as_u64().is_some_and(|count| count <= 512), "far grid rendered too many images: {far}");
                        assert_ne!(far["first"].as_str(), Some(initial_name.as_str()), "far grid must render a different page");
                        control
                            .eval("return (() => { const scroll = document.querySelector('.lc-grid-scroll'); if (!scroll) return false; scroll.scrollTop = 0; scroll.dispatchEvent(new Event('scroll', {bubbles: true})); return true; })();")
                            .expect("grid reset scroll command must execute");
                        wait_for_dom(control, "return Number.parseFloat(document.querySelector('.lc-grid-window')?.style.top || '0') === 0;");
                        let _descending = run(control, "library.sort", json!({"key": "fileName", "ascending": false}));
                        let descending_before = snapshot(control);
                        let descending_generation = descending_before["viewGeneration"].as_u64().expect("descending generation must be numeric");
                        assert!(descending_generation > generation, "descending sort must advance view generation");
                        wait_for_dom(control, &format!("return (() => {{ const name = document.querySelector('.lc-grid-window .lc-photo-caption span:first-child')?.textContent?.trim() || ''; return Boolean(name) && name !== {initial_name:?}; }})();"));
                        let descending_name = control
                            .eval("return document.querySelector('.lc-grid-window .lc-photo-caption span:first-child')?.textContent?.trim() || '';")
                            .expect("descending sort filename query must execute")
                            .as_str()
                            .unwrap_or_default()
                            .to_string();
                        assert!(!descending_name.is_empty(), "descending sort must expose first filename");
                        let _ascending = run(control, "library.sort", json!({"key": "fileName", "ascending": true}));
                        let ascending_snapshot = snapshot(control);
                        let ascending_generation = ascending_snapshot["viewGeneration"].as_u64().expect("ascending generation must be numeric");
                        assert!(ascending_generation > descending_generation, "ascending sort must advance view generation");
                        wait_for_dom(control, &format!("return (() => {{ const name = document.querySelector('.lc-grid-window .lc-photo-caption span:first-child')?.textContent?.trim() || ''; return Boolean(name) && name !== {descending_name:?}; }})();"));
                        let ascending_name = control
                            .eval("return document.querySelector('.lc-grid-window .lc-photo-caption span:first-child')?.textContent?.trim() || '';")
                            .expect("ascending sort filename query must execute")
                            .as_str()
                            .unwrap_or_default()
                            .to_string();
                        assert!(!ascending_name.is_empty(), "ascending sort must expose first filename");
                        assert_ne!(ascending_name, descending_name, "generation change must replace cached first page");
                        let stale = control
                            .command("lc_view_slice", &json!({"generation": descending_generation, "offset": 1536, "limit": 4096}))
                            .expect("stale scalability slice must reply");
                        assert_eq!(stale["generationChanged"].as_bool(), Some(true), "stale scalability generation must be rejected");
                        assert_eq!(stale["generation"].as_u64(), Some(ascending_generation));
                        let fresh = control
                            .command("lc_view_slice", &json!({"generation": ascending_generation, "offset": 1536, "limit": 512}))
                            .expect("fresh scalability tail must reply");
                        assert_eq!(fresh["generationChanged"].as_bool(), None);
                        assert_eq!(fresh["photos"].as_array().map(Vec::len), Some(512), "fresh tail page must remain capped at 512 photos");
                        assert_thumbnail_page_boundaries(control);
                    }
                    "gesture" => {
                        let gesture_photo = scenario.dir().join("gesture-photo.png");
                        fs::write(&gesture_photo, fixture_inputs::procedural_png(2048, 1365).expect("large gesture fixture must encode")).expect("large gesture fixture must be saved");
                        let imported = import_file(control, &gesture_photo);
                        let before = imported["undo"].as_u64().expect("snapshot undo count required");
                        control.key("D").expect("develop route key must execute");
                        wait_for_dom(control, "return document.querySelector('.stage-workspace.stage-detail') !== null;");
                        choose_inspector_panel(control, "edit");
                        wait_for_dom(control, r#"return document.querySelector("input[aria-label='Exposure']") !== null;"#);
                        let prepared = control
                            .eval(r#"return (() => {
                                const e = document.querySelector("input[aria-label='Exposure']");
                                if (!e) return {found: false};
                                e.scrollIntoView({block: 'center', inline: 'nearest'});
                                const r = e.getBoundingClientRect();
                                return {
                                    found: true,
                                    rect: {x: r.x, y: r.y, width: r.width, height: r.height},
                                    viewport: {width: innerWidth, height: innerHeight, dpr: devicePixelRatio},
                                };
                            })();"#)
                            .expect("exposure slider visibility query must execute");
                        assert_eq!(prepared["found"].as_bool(), Some(true), "exposure slider must exist before gesture");
                        assert_eq!(
                            wait_for_dom(control, r#"return (() => {
                                const e = document.querySelector("input[aria-label='Exposure']");
                                if (!e) return false;
                                const r = e.getBoundingClientRect();
                                return r.width > 1 && r.height > 1 && r.left >= 0 && r.top >= 0 &&
                                    r.right <= innerWidth && r.bottom <= innerHeight;
                            })();"#)
                            .as_bool(),
                            Some(true),
                            "exposure slider must be fully within native viewport",
                        );
                        let slider = control
                            .eval(r#"return (() => {
                                const e = document.querySelector("input[aria-label='Exposure']");
                                const r = e.getBoundingClientRect();
                                return {
                                    x: r.x, y: r.y, width: r.width, height: r.height,
                                    value: e.value, ariaValueNow: e.getAttribute('aria-valuenow'),
                                    viewport: {width: innerWidth, height: innerHeight, dpr: devicePixelRatio},
                                };
                            })();"#)
                            .expect("exposure slider geometry query must execute");
                        let x = slider["x"].as_f64().expect("exposure slider x must be numeric");
                        let y = slider["y"].as_f64().expect("exposure slider y must be numeric") + slider["height"].as_f64().unwrap_or(16.0) / 2.0;
                        let width = slider["width"].as_f64().expect("exposure slider width must be numeric");
                        assert!(x >= 0.0 && y >= 0.0 && x + width <= slider["viewport"]["width"].as_f64().unwrap_or(0.0), "exposure slider drag start must be within viewport: {slider}");
                        let diagnostic_setup = control
                            .eval(r#"return (() => {
                                const e = document.querySelector("input[aria-label='Exposure']");
                                if (!e) return {found: false};
                                const read = () => {
                                    const r = e.getBoundingClientRect();
                                    return {
                                        value: e.value,
                                        ariaValueNow: e.getAttribute('aria-valuenow'),
                                        rect: {x: r.x, y: r.y, width: r.width, height: r.height},
                                        viewport: {width: innerWidth, height: innerHeight, dpr: devicePixelRatio},
                                        visible: r.left >= 0 && r.top >= 0 && r.right <= innerWidth && r.bottom <= innerHeight,
                                    };
                                };
                                const state = window.__lcGestureDiagnostics || {events: [], bound: false};
                                if (!state.bound) {
                                    for (const type of ['pointerdown', 'pointermove', 'pointerup', 'pointercancel', 'input', 'change']) {
                                        e.addEventListener(type, event => {
                                            if (state.events.length < 64) state.events.push({
                                                type,
                                                trusted: event.isTrusted,
                                                clientX: event.clientX,
                                                clientY: event.clientY,
                                                buttons: event.buttons,
                                                value: e.value,
                                            });
                                        }, true);
                                    }
                                    state.bound = true;
                                }
                                state.before = read();
                                window.__lcGestureDiagnostics = state;
                                return {found: true, before: state.before, events: state.events};
                            })();"#)
                            .expect("exposure slider diagnostics setup must execute");
                        let drag_result = control.drag((x + width * 0.45, y), (x + width * 0.7, y), 8);
                        let after_drag = snapshot(control);
                        let mut edited = None;
                        let mut last_snapshot = after_drag.clone();
                        for _ in 0..100 {
                            let value = snapshot(control);
                            last_snapshot = value.clone();
                            if value["develop"]["light"]["exposure"] != imported["develop"]["light"]["exposure"]
                                && value["undo"].as_u64().is_some_and(|undo| undo > before)
                            {
                                edited = Some(value);
                                break;
                            }
                            sleep(Duration::from_millis(50));
                        }
                        let diagnostic_after = control
                            .eval(r#"return (() => {
                                const e = document.querySelector("input[aria-label='Exposure']");
                                const state = window.__lcGestureDiagnostics || {events: []};
                                if (!e) return {found: false, events: state.events};
                                const r = e.getBoundingClientRect();
                                return {
                                    found: true,
                                    value: e.value,
                                    ariaValueNow: e.getAttribute('aria-valuenow'),
                                    rect: {x: r.x, y: r.y, width: r.width, height: r.height},
                                    viewport: {width: innerWidth, height: innerHeight, dpr: devicePixelRatio},
                                    visible: r.left >= 0 && r.top >= 0 && r.right <= innerWidth && r.bottom <= innerHeight,
                                    events: state.events,
                                };
                            })();"#)
                            .expect("exposure slider diagnostics readback must execute");
                        let compact_host = |value: &Value| {
                            json!({
                                "exposure": value.pointer("/develop/light/exposure").cloned().unwrap_or(Value::Null),
                                "undo": value.get("undo").cloned().unwrap_or(Value::Null),
                                "active": value.get("active").cloned().unwrap_or(Value::Null),
                                "viewGeneration": value.get("viewGeneration").cloned().unwrap_or(Value::Null),
                                "status": {"error": value.pointer("/status/error").cloned().unwrap_or(Value::Null)},
                            })
                        };
                        let events = diagnostic_after
                            .get("events")
                            .and_then(Value::as_array)
                            .map(|values| values.iter().take(64).cloned().collect::<Vec<_>>())
                            .unwrap_or_default();
                        let receipt = scenario.dir().join("gesture-pointer.json");
                        let receipt_value = json!({
                            "slider": slider,
                            "prepared": prepared,
                            "setup": diagnostic_setup.get("before").cloned().unwrap_or(Value::Null),
                            "after": diagnostic_after,
                            "events": events,
                            "host": {
                                "before": compact_host(&imported),
                                "afterDrag": compact_host(&after_drag),
                                "after": compact_host(&last_snapshot),
                            },
                        });
                        if let Ok(bytes) = serde_json::to_vec_pretty(&receipt_value) {
                            let _ = fs::write(&receipt, bytes);
                        }
                        eprintln!("[qa] gesture pointer receipt={}", receipt.display());
                        drag_result.expect("exposure slider drag must execute");
                        let edited = edited.unwrap_or_else(|| panic!("pointer drag edit did not settle through host bridge; receipt={}", receipt.display()));
                        assert_ne!(edited["develop"]["light"]["exposure"], imported["develop"]["light"]["exposure"], "pointer drag must change exposure through UI");
                        assert_eq!(edited["undo"].as_u64(), Some(before + 1), "one pointer gesture must create one undo step");
                        let edited_exposure = edited["develop"]["light"]["exposure"].clone();
                        let focused = control
                            .eval(r#"return (() => { const node = document.querySelector("input[aria-label='Exposure']"); if (!node) return false; node.focus(); return document.activeElement === node; })();"#)
                            .expect("exposure slider focus query must execute");
                        assert_eq!(focused.as_bool(), Some(true), "exposure slider focus must execute");
                        control.eval("window.__lcInteractionDiagnostics = []; return true;").expect("keyboard interaction diagnostics must initialize");
                        let keyboard_result = catch_unwind(AssertUnwindSafe(|| {
                            control.key("Left").expect("exposure keyboard adjustment must execute");
                            wait_for_snapshot(
                                control,
                                |value| value["develop"]["light"]["exposure"] != edited_exposure,
                                "keyboard adjustment must reach engine before testing cancellation",
                            );
                            control.key("Escape").expect("exposure Escape cancellation must execute");
                            let cancelled = wait_for_snapshot(
                                control,
                                |value| {
                                    value["develop"]["light"]["exposure"] == edited_exposure
                                        && value["undo"].as_u64() == edited["undo"].as_u64()
                                },
                                "Escape cancellation did not settle through host bridge",
                            );
                            assert_eq!(cancelled["develop"]["light"]["exposure"], edited_exposure, "Escape must cancel active slider gesture");
                            assert_eq!(cancelled["undo"].as_u64(), edited["undo"].as_u64(), "cancelled gesture must not create undo step");
                        }));
                        let keyboard_trace = control.eval("return (() => { const trace = window.__lcInteractionDiagnostics || []; delete window.__lcInteractionDiagnostics; return trace; })();").unwrap_or(Value::Null);
                        let keyboard_receipt = json!({"before": edited, "after": snapshot(control), "trace": keyboard_trace});
                        if let Ok(bytes) = serde_json::to_vec_pretty(&keyboard_receipt) {
                            let _ = fs::write(scenario.dir().join("gesture-keyboard.json"), bytes);
                        }
                        if let Err(payload) = keyboard_result {
                            resume_unwind(payload);
                        }
                        assert_sustained_preview_gestures(control, &scenario.dir().join("gesture-preview-stress.json"));
                        control.move_to(300.0, 300.0).expect("pointer move must execute");
                        control.drag((300.0, 300.0), (420.0, 320.0), 8).expect("pointer drag must execute");
                        control.wheel(420.0, 320.0, 0.0, -120.0).expect("wheel must execute");
                        control.key("Escape").expect("Escape must execute");
                        assert!(
                            control
                                .eval("return document.activeElement !== null;")
                                .expect("active element query must execute")
                                .as_bool()
                                .unwrap_or(false)
                        );
                    }
                    "editingTools" => {
                        let imported = import_file(control, &inputs.png);
                        let active = imported["active"].as_u64().expect("import must select active photo");
                        let before = snapshot(control);
                        control.key("D").expect("develop route key must execute");
                        wait_for_dom(control, "return document.querySelector('.stage-workspace.stage-detail') !== null;");
                        run(control, "crop.set", json!({"rect": [0.1, 0.1, 0.9, 0.9], "angle": 3.0}));
                        let crop = snapshot(control);
                        assert_ne!(crop["develop"]["crop"], before["develop"]["crop"], "crop command must change crop geometry");
                        choose_inspector_panel(control, "crop");
                        wait_for_dom(control, r#"return document.querySelector(".lc-inspector button[data-panel='crop']")?.getAttribute('aria-pressed') === 'true';"#);
                        wait_for_rendered_preview(control, "img.stage-preview", None);
                        capture_visible_tool_preview(control, &scenario.dir().join("tool-crop.png"));

                        run(control, "mask.add", json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.2, "ry": 0.2}));
                        let masked = snapshot(control);
                        assert_eq!(masked["develop"]["masks"].as_array().map(Vec::len), Some(1), "mask command must create mask state");
                        choose_inspector_panel(control, "masking");
                        wait_for_dom(control, r#"return document.querySelector(".lc-inspector button[data-panel='masking']")?.getAttribute('aria-pressed') === 'true';"#);
                        wait_for_rendered_preview(control, "img.stage-preview", None);
                        wait_for_dom(control, "return document.querySelector('.lc-inspector__header small')?.textContent === '1 mask';");
                        capture_visible_tool_preview(control, &scenario.dir().join("tool-masking.png"));

                        click_dom(control, ".lc-mask-modes > button:first-child", "Brush must select its own mask through actual UI");
                        let brush = wait_for_snapshot(control, |value| value["develop"]["masks"].as_array().map(Vec::len) == Some(2), "Brush UI must create second mask");
                        let brush_id = brush["activeMask"].as_u64().expect("selected brush mask must be authoritative");
                        let geometry = control.eval("return (() => { const e = document.querySelector('.single-pane .stage-image-wrap'); if (!e) return null; const r = e.getBoundingClientRect(); return {x:r.x,y:r.y,w:r.width,h:r.height}; })();").expect("brush stage must expose pointer bounds");
                        let x = geometry["x"].as_f64().expect("brush stage x");
                        let y = geometry["y"].as_f64().expect("brush stage y");
                        let w = geometry["w"].as_f64().expect("brush stage width");
                        let h = geometry["h"].as_f64().expect("brush stage height");
                        control.drag((x + w * 0.45, y + h * 0.45), (x + w * 0.65, y + h * 0.55), 8).expect("actual brush pointer stroke must execute");
                        let painted = wait_for_snapshot(control, |value| {
                            value["develop"]["masks"].as_array().is_some_and(|masks| masks.iter().any(|mask| {
                                mask["id"].as_u64() == Some(brush_id) && mask["components"].as_array().is_some_and(|components| components.iter().any(|component| {
                                    component["shape"]["strokes"].as_array().is_some_and(|strokes| !strokes.is_empty())
                                }))
                            }))
                        }, "brush stroke must reach selected engine mask");
                        assert_eq!(painted["activeMask"].as_u64(), Some(brush_id));
                        wait_for_dom(control, "return document.querySelector('.mask-vector .mask-brush-path') !== null;");
                        run(control, "edit.undo", json!({}));
                        assert_eq!(snapshot(control)["develop"]["masks"], brush["develop"]["masks"], "one undo must remove whole brush gesture");
                        run(control, "edit.redo", json!({}));
                        assert_eq!(snapshot(control)["develop"]["masks"], painted["develop"]["masks"], "redo must restore brush gesture");
                        capture_visible_tool_preview(control, &scenario.dir().join("tool-brush-pointer.png"));

                        run(control, "spot.add", json!({"mode": "remove", "points": [[0.5, 0.5]], "size": 0.05, "source": [0.1, 0.0]}));
                        let spotted = snapshot(control);
                        assert_eq!(spotted["develop"]["spots"].as_array().map(Vec::len), Some(1), "remove tool command must create spot state");
                        choose_inspector_panel(control, "remove");
                        wait_for_dom(control, r#"return document.querySelector(".lc-inspector button[data-panel='remove']")?.getAttribute('aria-pressed') === 'true';"#);
                        wait_for_rendered_preview(control, "img.stage-preview", None);
                        capture_visible_tool_preview(control, &scenario.dir().join("tool-remove.png"));

                        run(control, "redeye.add", json!({"center": [0.5, 0.5], "rx": 0.1, "ry": 0.1}));
                        let red_eye = snapshot(control);
                        assert_eq!(red_eye["develop"]["red_eye"].as_array().map(Vec::len), Some(1), "red-eye command must create correction state");
                        choose_inspector_panel(control, "redeye");
                        wait_for_dom(control, r#"return document.querySelector(".lc-inspector button[data-more-tools]")?.getAttribute('aria-pressed') === 'true';"#);
                        wait_for_rendered_preview(control, "img.stage-preview", None);
                        capture_visible_tool_preview(control, &scenario.dir().join("tool-red-eye.png"));
                        assert_eq!(red_eye["active"].as_u64(), Some(active));
                    }
                    "importFilters" => {
                        let folder = inputs.png.parent().expect("media folder must exist");
                        let png_only = import_review(control, json!({"paths": [folder], "includeSubfolders": false, "allowedExtensions": [" .PNG "]}));
                        let candidates = png_only["candidates"].as_array().expect("filtered review must return candidates");
                        assert_eq!(candidates.len(), 1, "PNG filter must exclude ARW: {png_only}");
                        let provisional = &candidates[0]["preview"];
                        assert_eq!(provisional["state"], "provisional", "review must show quick preview before import");
                        assert!(provisional["handle"].as_str().is_some_and(|handle| !handle.is_empty()));
                        assert!(provisional["width"].as_u64().is_some_and(|width| width > 0 && width <= 256));
                        control.command("lc_preview_ack", &json!({"handle": provisional["handle"]})).expect("review preview lease must release");
                        let raw_only = import_review(control, json!({"paths": [folder], "excludedExtensions": ["png"]}));
                        assert_eq!(raw_only["candidates"].as_array().map(Vec::len), Some(1), "exclusion must leave ARW");
                        let explicit = import_review(control, json!({"paths": [inputs.png], "allowedExtensions": ["arw"]}));
                        assert_eq!(explicit["candidates"].as_array().map(Vec::len), Some(1), "explicit chosen file must be retained");
                        fs::write(scenario.dir().join("import-filters.json"), serde_json::to_vec_pretty(&json!({"png": png_only, "raw": raw_only, "explicit": explicit})).expect("filter evidence must serialize")).expect("filter evidence must save");
                    }
                    "modelSetup" => {
                        let folder = scenario.dir().join("invalid-model");
                        fs::create_dir_all(&folder).expect("malformed model folder must stage");
                        fs::write(folder.join("model.safetensors"), b"invalid model").expect("malformed weights must stage");
                        let before = run(control, "segment.model.status", Value::Null);
                        let started = run(control, "segment.model.selectFolder", json!({"path": folder}));
                        assert_eq!(started["pending"].as_bool(), Some(true), "folder selection must start background validation");
                        let responsive = snapshot(control);
                        assert!(responsive["revision"].is_number(), "owner snapshot must remain responsive during validation");
                        let mut finished = Value::Null;
                        for _ in 0..100 {
                            finished = run(control, "segment.model.status", Value::Null);
                            if finished["validation"]["finished"].as_bool() == Some(true) { break; }
                            sleep(Duration::from_millis(50));
                        }
                        assert_eq!(finished["validation"]["finished"].as_bool(), Some(true));
                        assert!(finished["validation"]["error"].as_str().is_some_and(|error| !error.is_empty()), "malformed model must report actionable failure");
                        assert_eq!(finished["dir"], before["dir"], "failed validation must preserve configured model folder");
                        fs::write(scenario.dir().join("model-validation.json"), serde_json::to_vec_pretty(&finished).expect("validation evidence must serialize")).expect("validation evidence must save");
                    }
                    "arwImport" => {
                        let imported = import_file(control, &inputs.arw);
                        control.key("G").expect("library route key must execute");
                        wait_for_dom(control, "return document.querySelector('.lc-library-workspace') !== null;");
                        wait_for_rendered_preview(control, ".lc-photo-cell.is-active img.lc-photo-preview", None);
                        assert_active_grid_identity(control, "synthetic-sonya-01.arw");
                        let descriptor = preview_imported(control, &imported);
                        control.key("D").expect("develop route key must execute");
                        wait_for_dom(control, "return document.querySelector('.stage-workspace.stage-detail') !== null;");
                        let rendered = wait_for_rendered_preview(control, "img.stage-preview", None);
                        let screenshot = scenario.dir().join("arw-preview.png");
                        control.screenshot_to(&screenshot).expect("ARW preview screenshot must be captured");
                        assert!(screenshot.is_file());
                        assert!(descriptor["width"].as_u64().is_some_and(|width| width > 0));
                        assert!(rendered["naturalWidth"].as_u64().is_some_and(|width| width > 0));
                        assert!(rendered["naturalHeight"].as_u64().is_some_and(|height| height > 0));
                    }
                    "lightroomImport" => {
                        let inspected = task_result(control, "library.inspectLightroom", json!({"path": inputs.catalog}));
                        assert_eq!(inspected["photos"].as_u64(), Some(2));
                        let first = task_result(control, "library.importLightroom", json!({"path": inputs.catalog, "updateExisting": false}));
                        assert_eq!(first["photos"].as_u64(), Some(2), "Lightroom fixture must import master & virtual copy: {first}");
                        assert_eq!(first["collections"].as_u64(), Some(2), "nested Lightroom collections must import: {first}");
                        assert_eq!(first["missing"].as_array().map(Vec::len), Some(0));
                        let mapping = first["mapping"].clone();
                        let second = task_result(control, "library.importLightroom", json!({"path": inputs.catalog, "updateExisting": true}));
                        assert_eq!(second["mapping"], mapping, "reimport must preserve source identities");
                        assert_eq!(second["archive"], first["archive"], "unchanged source must reuse bounded archive");
                        fs::write(scenario.dir().join("lightroom-import.json"), serde_json::to_vec_pretty(&json!({"inspection": inspected, "first": first, "reimport": second})).expect("Lightroom receipt must serialize")).expect("Lightroom receipt must save");
                        assert_eq!(snapshot(control)["counts"]["catalog"].as_u64(), Some(2));
                    }
                    "engineExport" => {
                        click_dom(control, "button[data-nav-id=\"all\"]", "All Photos must open library");
                        wait_for_dom(control, "return document.querySelector('.lc-header-action') !== null;");
                        click_dom(control, ".lc-header-action", "Import must open review before asking for a native picker");
                        wait_for_dom(control, "return document.querySelector('#lc-dialog-title')?.textContent === 'Import Photos';");
                        assert_eq!(snapshot(control)["status"]["jobs"].as_array().map(Vec::len), Some(0), "opening Import must not start a scan or block library owner");
                        control.key("escape").expect("empty import dialog must dismiss");
                        wait_for_dom(control, "return document.querySelector('.lc-dialog') === null;");
                        let picker = control.command("lc_native", &json!({"action": "pickFiles", "params": {}}));
                        assert!(picker.is_err(), "background control must refuse interactive pickers");
                        for action in [
                            "openLibrary",
                            "backupLibrary",
                            "restoreLibrary",
                            "selectFolder",
                            "openFile",
                            "saveFile",
                            "pickPresetFiles",
                            "pickCurvePresetFiles",
                            "pickTracklog",
                            "savePresetFile",
                            "saveCurvePresetFile",
                        ] {
                            let result = control.command("lc_native", &json!({"action": action, "params": {}}));
                            assert!(result.is_err(), "background control must reject {action} without explicit path input");
                        }
                        let explicit = control.command("lc_native", &json!({"action": "pickFiles", "params": {"paths": [inputs.png.to_string_lossy()]}})).expect("background picker must accept explicit paths");
                        assert_eq!(explicit["paths"].as_array().map(Vec::len), Some(1));
                        let imported = import_file(control, &inputs.png);
                        let id = imported["active"].as_u64().expect("PNG import must select photo");
                        control.key("D").expect("develop route key must execute for engine export journey");
                        wait_for_dom(control, "return document.querySelector('.stage-workspace.stage-detail') !== null;");
                        choose_inspector_panel(control, "edit");
                        wait_for_dom(control, "return document.querySelector('.lc-histogram svg path[stroke=\"#df6464\"]')?.getAttribute('d')?.includes('L') === true && document.querySelector('.lc-histogram__footer')?.textContent.includes('samples') === true;");
                        let curve_before = snapshot(control)["develop"]["curve"].clone();
                        let curve_heading = control.eval("return (() => { const button = [...document.querySelectorAll('.lc-inspector__section-head')].find(node => node.querySelector('span:nth-child(2)')?.textContent.trim() === 'Tone Curve'); if (!button) return null; button.scrollIntoView({block: 'center'}); const rect = button.getBoundingClientRect(); return {x: rect.x + rect.width / 2, y: rect.y + rect.height / 2, open: button.getAttribute('data-open') === 'true'}; })();").expect("Tone Curve section must expose its actual heading");
                        if curve_heading["open"].as_bool() != Some(true) {
                            control.click(curve_heading["x"].as_f64().expect("curve heading x must be numeric"), curve_heading["y"].as_f64().expect("curve heading y must be numeric"), "left", 1).expect("collapsed Tone Curve section must open by pointer");
                        }
                        wait_for_dom(control, "return document.querySelector('.lc-curve-picker > button') !== null;");
                        let scrolled = control.eval("return (() => { const button = document.querySelector('.lc-curve-picker > button'); if (!button) return false; button.scrollIntoView({block: 'center', inline: 'nearest'}); return true; })();").expect("curve preset control must scroll into view");
                        assert_eq!(scrolled.as_bool(), Some(true));
                        wait_for_dom(control, "return (() => { const button = document.querySelector('.lc-curve-picker > button'); if (!button) return false; const rect = button.getBoundingClientRect(); return rect.top >= 0 && rect.bottom <= innerHeight && rect.left >= 0 && rect.right <= innerWidth; })();");
                        click_dom(control, ".lc-curve-picker > button", "curve preset picker must open");
                        wait_for_dom(control, "return document.querySelector('[aria-label=\"Tone curve presets\"]') !== null;");
                        click_dom(control, ".lc-curve-picker__menu [role=menuitemradio]:nth-child(3)", "Strong Contrast preset must apply");
                        let curve_changed = wait_for_snapshot(control, |state| state["develop"]["curve"] != curve_before, "curve preset must change Rust develop settings");
                        assert_ne!(curve_changed["develop"]["curve"], curve_before);
                        run(control, "edit.undo", json!({}));
                        assert_eq!(snapshot(control)["develop"]["curve"], curve_before, "curve preset must be undoable");
                        let decoded_before = wait_for_rendered_preview(control, "img.stage-preview", None);
                        assert!(decoded_before["naturalWidth"].as_u64().is_some_and(|width| width > 0));
                        assert!(decoded_before["naturalHeight"].as_u64().is_some_and(|height| height > 0));
                        let decoded_before_src = decoded_before["src"].as_str().expect("decoded stage preview must expose source handle").to_string();
                        let before = snapshot(control);
                        let before_exposure = before["develop"]["light"]["exposure"].clone();
                        let before_undo = before["undo"].as_u64().expect("engine export journey must expose undo count");
                        run(control, "develop.beginInteraction", json!({"label": "Qualification edit"}));
                        run(control, "develop.set", json!({"control": "light.exposure", "value": 0.75, "ids": [id]}));
                        run(control, "develop.endInteraction", json!({}));
                        let edited = snapshot(control);
                        assert_ne!(edited["develop"]["light"]["exposure"], before_exposure, "develop.set must change exposure state");
                        assert!(edited["undo"].as_u64().is_some_and(|undo| undo > before_undo), "develop.set must create undo state");
                        let decoded_edited = wait_for_rendered_preview(control, "img.stage-preview", Some(&decoded_before_src));
                        let decoded_edited_src = decoded_edited["src"].as_str().expect("edited stage preview must expose source handle").to_string();
                        run(control, "edit.undo", json!({}));
                        let undone = snapshot(control);
                        assert_eq!(undone["develop"]["light"]["exposure"], before_exposure, "edit.undo must restore exposure state");
                        assert_eq!(undone["undo"].as_u64(), Some(before_undo), "edit.undo must remove qualification edit");
                        let decoded_undone = wait_for_rendered_preview(control, "img.stage-preview", Some(&decoded_edited_src));
                        let screenshot = scenario.dir().join("engine-export-stage.png");
                        control.screenshot_to(&screenshot).expect("engine export stage screenshot must be captured");
                        assert!(screenshot.is_file());
                        let output = scenario.dir().join("export");
                        fs::create_dir_all(&output).expect("export directory must exist");
                        let started = run(control, "app.export", json!({"ids": [id], "dir": output, "format": "png", "longEdge": 96}));
                        let task_id = started["taskId"].as_str().expect("export must return task id").to_string();
                        wait_task(control, &task_id);
                        let exported = fs::read_dir(&output)
                            .expect("export output must exist")
                            .filter_map(Result::ok)
                            .map(|entry| entry.path())
                            .find(|path| path.is_file())
                            .expect("export must write file");
                        assert_png_pixels(&exported, 96, 64);
                        // Completed work remains inspectable after its initiating UI closes.
                        click_dom(control, ".lc-background-activity > button", "background activity must open");
                        let task_selector = format!("#lc-background-jobs [data-task-id=\"{task_id}\"]");
                        wait_for_dom(control, &format!("return document.querySelector({})?.getAttribute('data-task-state') === 'done';", serde_json::to_string(&task_selector).expect("task selector must serialize")));
                        let activity = control.eval("return (() => { const panel = document.querySelector('#lc-background-jobs'); if (!panel) return null; const r = panel.getBoundingClientRect(); return { text: panel.textContent, inside: r.left >= 0 && r.right <= innerWidth && r.top >= 0 && r.bottom <= innerHeight, reachable: panel.contains(document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2)) }; })();").expect("background activity must expose geometry");
                        assert_eq!(activity["inside"].as_bool(), Some(true), "background activity must stay within viewport: {activity}");
                        assert_eq!(activity["reachable"].as_bool(), Some(true), "background activity must not be obscured: {activity}");
                        control.screenshot_to(&scenario.dir().join("background-activity.png")).expect("background activity screenshot must save");
                        // Task completion removes its Cancel button, returning focus to body.
                        let focus_lost = control.eval("document.activeElement?.blur(); return document.activeElement === document.body;").expect("task focus-loss state must be reproducible");
                        assert_eq!(focus_lost.as_bool(), Some(true), "task completion focus loss must be exercised");
                        control.key("escape").expect("background activity must dismiss with Escape");
                        wait_for_dom(control, "return document.querySelector('#lc-background-jobs') === null && document.activeElement === document.querySelector('.lc-background-activity > button');");
                        // Exercise actual React dialog, native IPC & encoder, rather than
                        // proving only engine export with hand-authored command parameters.
                        click_dom(control, ".rk-search--trigger", "Export palette trigger must execute");
                        wait_for_dom(control, "return document.querySelector('.rk-palette') !== null;");
                        for key in ["e", "x", "p", "o", "r", "t"] {
                            control.key(key).expect("Export palette search must execute");
                        }
                        wait_for_dom(control, "return document.querySelector('[data-command=\"dialog.export\"]') !== null;");
                        click_dom(control, "[data-command=\"dialog.export\"]", "Export command must open real dialog");
                        wait_for_dom(control, "return document.querySelector('.lc-dialog h2')?.textContent === 'Export';");
                        set_dialog_field(control, "Export folder", &output.display().to_string());
                        set_dialog_field(control, "Format", "png");
                        set_dialog_field(control, "Size", "96");
                        set_dialog_field(control, "Naming template", "dialog-{name}");
                        set_dialog_field(control, "Sharpen", "screen");
                        wait_for_dom(control, "return [...document.querySelectorAll('.lc-dialog .lc-field span')].some((node) => node.textContent === 'Sharpen amount');");
                        set_dialog_field(control, "Sharpen amount", "high");
                        assert_eq!(control.eval("return (() => { const node = [...document.querySelectorAll('.lc-dialog .lc-check')].find((item) => item.querySelector('span')?.textContent === 'Watermark')?.querySelector('input'); if (!node) return false; node.click(); return node.checked; })();").expect("watermark checkbox must execute").as_bool(), Some(true));
                        wait_for_dom(control, "return document.querySelector('.lc-dialog textarea') !== null;");
                        set_dialog_field(control, "Watermark text", "MMMM\nMMMM");
                        set_dialog_field(control, "Size (%)", "15");
                        set_dialog_field(control, "Opacity (%)", "100");
                        set_dialog_field(control, "Colour", "#ff00ff");
                        set_dialog_field(control, "Watermark position", "topLeft");
                        let dialog_fields = control.eval("return Object.fromEntries([...document.querySelectorAll('.lc-dialog .lc-field')].map((field) => [field.querySelector('span')?.textContent, field.querySelector('input,select,textarea')?.value]));").expect("actual Export fields must be recorded");
                        assert_eq!(dialog_fields["Watermark text"].as_str(), Some("MMMM\nMMMM"), "multiline watermark must survive React input");
                        assert_eq!(dialog_fields["Sharpen amount"].as_str(), Some("high"));
                        let dialog_screenshot = scenario.dir().join("export-dialog.png");
                        control.screenshot_to(&dialog_screenshot).expect("actual Export dialog screenshot must save");
                        click_dom(control, ".lc-dialog-actions .lc-button-primary", "real Export submit must execute");
                        let dialog_exported = output.join("dialog-procedural-rgb-01.png");
                        let export_deadline = Instant::now() + Duration::from_secs(30);
                        while !dialog_exported.is_file() && Instant::now() < export_deadline {
                            sleep(Duration::from_millis(50));
                        }
                        assert!(dialog_exported.is_file(), "React Export dialog must create named PNG; native state={}", snapshot(control));
                        assert_png_pixels(&dialog_exported, 96, 64);
                        let baseline_magenta = top_left_magenta_pixels(&exported);
                        let watermark_magenta = top_left_magenta_pixels(&dialog_exported);
                        assert!(watermark_magenta >= baseline_magenta + 5, "React watermark must add visible top-left magenta pixels: before={baseline_magenta}, after={watermark_magenta}");
                        let receipt = scenario.dir().join("engine-export.json");
                        fs::write(
                            &receipt,
                            serde_json::to_vec_pretty(&json!({
                                "schema": 1,
                                "journey": "import-decoded-preview-edit-undo-export",
                                "photoId": id,
                                "decodedPreviewBefore": decoded_before,
                                "decodedPreviewEdited": decoded_edited,
                                "decodedPreviewUndone": decoded_undone,
                                "beforeExposure": before_exposure.clone(),
                                "editedExposure": edited["develop"]["light"]["exposure"].clone(),
                                "undoRestored": undone["develop"]["light"]["exposure"] == before_exposure,
                                "exportedPng": exported,
                                "dialogFields": dialog_fields,
                                "dialogExportedPng": dialog_exported,
                                "baselineTopLeftMagentaPixels": baseline_magenta,
                                "watermarkedTopLeftMagentaPixels": watermark_magenta,
                            }))
                            .expect("engine export receipt must serialize"),
                        )
                        .expect("engine export receipt must be writable");
                        scenario.keep("engine-export-stage.png", &screenshot);
                        scenario.keep("engine-export.json", &receipt);
                        scenario.keep("export-dialog.png", &dialog_screenshot);
                        scenario.keep("dialog-export.png", &dialog_exported);
                    }
                    _ => unreachable!("scenario inventory is static"),
                });
            }
            scenario.note(format!("executed hidden {platform} control journey: {name}"));
        })
        }));
        match outcome {
            Ok(outcome) => assert!(!outcome.is_skipped(), "native journey {name} must execute, not skip"),
            Err(_) => failures.push(name),
        }
        assert_sources_unchanged(&source_paths, &source_hashes);
    }
    assert!(failures.is_empty(), "native journeys failed: {}", failures.join(", "));
}
