import { canonicalSection, sectionIsOpen, toggledSection } from '../controlSections';
import { useEffect, useMemo, useRef, useState, type RefObject } from 'react';
import type { ControlSpec, DesktopSnapshot, JsonObject, Panel, UiState } from '../types';
import { useDesktop } from '../desktop';
import { Icon, type IconName } from '../icons';
import { Checkbox, ContextMenu, FuseCurves, Select, Slider, type MenuItem } from '@rightkit/react-shell/react';
import { SliderCommandQueue } from './sliderCommandQueue';
import { fromRange, rangeProps } from './trackScale';
import './Inspector.css';

type Run = (id: string, params?: JsonObject) => Promise<unknown>;
type Json = Record<string, unknown>;
type InspectorDesktop = {
  snapshot: DesktopSnapshot | null;
  ui: UiState;
  setUi: (patch: Partial<UiState> | ((ui: UiState) => Partial<UiState>)) => void;
  run: Run;
  histogram: unknown;
  setHistogram: (histogram: unknown) => void;
};

type InteractionDiagnostic = Record<string, unknown>;
type DiagnosticWindow = Window & { __lcInteractionDiagnostics?: InteractionDiagnostic[] };

const PANEL_ITEMS: Array<{ id: Exclude<Panel, null>; label: string; icon: IconName }> = [
  { id: 'edit', label: 'Edit', icon: 'develop' },
  { id: 'crop', label: 'Crop', icon: 'crop' },
  { id: 'remove', label: 'Heal', icon: 'remove' },
  { id: 'masking', label: 'Mask', icon: 'masking' },
  { id: 'presets', label: 'Presets', icon: 'presets' },
  { id: 'info', label: 'Info', icon: 'info' },
];

const MORE_PANEL_ITEMS: Array<{ id: Exclude<Panel, null>; label: string; icon: IconName }> = [
  { id: 'profiles', label: 'Profiles', icon: 'profiles' },
  { id: 'redeye', label: 'Red eye', icon: 'redeye' },
  { id: 'versions', label: 'Versions', icon: 'versions' },
  { id: 'activity', label: 'History', icon: 'history' },
  { id: 'keywords', label: 'Keywords', icon: 'keywords' },
];

const SECTION_LABELS: Record<string, string> = {
  Light: 'Light', Curve: 'Tone Curve', Color: 'Color', Mixer: 'Color Mixer', BwMix: 'B&W Mixer',
  Grading: 'Color Grading', Effects: 'Effects', Vignette: 'Vignette', Grain: 'Grain', Detail: 'Detail',
  Optics: 'Optics', Geometry: 'Geometry', Profile: 'Profile', Calibration: 'Calibration', PointColor: 'Point Color', RedEye: 'Red Eye',
  light: 'Light', curve: 'Tone Curve', color: 'Color', mixer: 'Color Mixer', bwMix: 'B&W Mixer', grading: 'Color Grading', effects: 'Effects', vignette: 'Vignette', grain: 'Grain', detail: 'Detail', optics: 'Optics', geometry: 'Geometry', profile: 'Profile', calibration: 'Calibration', pointColor: 'Point Color', redEye: 'Red Eye',
};

const SECTION_ORDER = ['Light', 'Color', 'Exposure', 'Tone Curve', 'Color Mixer', 'B&W Mixer', 'Color Grading', 'Effects', 'Vignette', 'Grain', 'Detail', 'Optics', 'Geometry', 'Profile', 'Calibration', 'Point Color', 'Red Eye'];

function object(value: unknown): Json { return value && typeof value === 'object' && !Array.isArray(value) ? value as Json : {}; }
function array(value: unknown): unknown[] { return Array.isArray(value) ? value : []; }
function text(value: unknown, fallback = ''): string { return typeof value === 'string' ? value : fallback; }
function num(value: unknown, fallback = 0): number { return typeof value === 'number' && Number.isFinite(value) ? value : fallback; }

const HISTOGRAM_BINS = 256;

function histogramBins(value: unknown): number[] {
  const source = array(value);
  return Array.from({ length: HISTOGRAM_BINS }, (_, index) => {
    const bin = source[index];
    return typeof bin === 'number' && Number.isFinite(bin) && bin >= 0 ? bin : 0;
  });
}

function curvePresetResult(value: unknown): { presets: Json[]; current: string | null } {
  const result = object(value);
  const presets = array(result.presets).map(object).filter(preset => text(preset.name).trim().length > 0);
  const current = text(result.current).trim();
  return { presets, current: current || null };
}

export default function Inspector({ className = '' }: { className?: string }) {
  const desktop = useDesktop() as InspectorDesktop;
  const { snapshot, ui } = desktop;
  const theme = ui.theme === 'light' || ui.theme === 'dark' ? ui.theme : undefined;
  const panel = ui.panel ?? (ui.view === 'detail' ? 'edit' : 'info');
  const [moreOpen, setMoreOpen] = useState(false);
  const moreButtonRef = useRef<HTMLButtonElement>(null);
  const choose = (next: Exclude<Panel, null>) => {
    desktop.setUi({ inspectorCollapsed: false, panel: next, ...(next === 'edit' || next === 'crop' || next === 'remove' || next === 'masking' || next === 'redeye' || next === 'profiles' || next === 'presets' ? { view: 'detail' } : {}) });
  };
  const hiddenPanelActive = MORE_PANEL_ITEMS.some(item => item.id === panel);
  return <aside className={`lc-inspector ${ui.inspectorCollapsed ? 'is-collapsed' : ''} ${className}`} data-theme={theme} aria-label="Inspector">
    <div className="lc-inspector__content">
      <div className="lc-inspector__scroll">
      {panel === 'edit' && <EditPanel desktop={desktop} />}
      {panel === 'profiles' && <ProfilesPanel desktop={desktop} />}
      {panel === 'crop' && <CropPanel desktop={desktop} />}
      {panel === 'remove' && <RemovePanel desktop={desktop} />}
      {panel === 'masking' && <MaskingPanel desktop={desktop} />}
      {panel === 'redeye' && <RedEyePanel desktop={desktop} />}
      {panel === 'presets' && <PresetsPanel desktop={desktop} />}
      {panel === 'versions' && <VersionsPanel desktop={desktop} />}
      {panel === 'activity' && <HistoryPanel desktop={desktop} />}
      {panel === 'keywords' && <KeywordsPanel desktop={desktop} />}
      {panel === 'info' && <InfoPanel desktop={desktop} />}
      {!snapshot && <div className="lc-inspector__empty">Waiting for desktop session…</div>}
      </div>
    </div>
    <ToolRail active={panel} choose={choose} open={moreOpen} setOpen={setMoreOpen} hiddenActive={hiddenPanelActive} buttonRef={moreButtonRef} collapsed={ui.inspectorCollapsed} toggleCollapsed={() => desktop.setUi({ inspectorCollapsed: !ui.inspectorCollapsed })} />
  </aside>;
}

function ToolRail({ active, choose, open, setOpen, hiddenActive, buttonRef, collapsed, toggleCollapsed }: { active: Panel; choose: (panel: Exclude<Panel, null>) => void; open: boolean; setOpen: (open: boolean) => void; hiddenActive: boolean; buttonRef: RefObject<HTMLButtonElement | null>; collapsed: boolean; toggleCollapsed: () => void }) {
  const moreItems: MenuItem[] = MORE_PANEL_ITEMS.map(item => ({ id: item.id, label: item.label, icon: <Icon name={item.icon} size={16} />, checked: active === item.id, run: () => choose(item.id) }));
  return <nav className="lc-inspector__rail" aria-label="Inspector tools">
    <div className="lc-inspector__rail-tools">
      {PANEL_ITEMS.map(item => <button key={item.id} type="button" className="lc-inspector__tool" data-panel={item.id} aria-label={item.label} aria-pressed={active === item.id} title={item.label} onClick={() => choose(item.id)}><span className="lc-inspector__tool-icon" aria-hidden="true"><Icon name={item.icon} size={17} /></span>{active === item.id && <FuseCurves fill="var(--rk-plane)" />}</button>)}
    </div>
    <div className="lc-inspector__rail-more">
      <button ref={buttonRef} type="button" className="lc-inspector__tool" data-more-tools="true" aria-label="More tools" aria-haspopup="menu" aria-expanded={open} aria-pressed={hiddenActive} title="More tools" onClick={() => setOpen(!open)}><span className="lc-inspector__tool-icon" aria-hidden="true"><span className="lc-inspector__more-glyph">•••</span></span>{hiddenActive && <FuseCurves fill="var(--rk-plane)" />}</button>
      {open && buttonRef.current && <ContextMenu anchor={buttonRef.current} placement="top-end" items={moreItems} label="More tools" width={176} onClose={() => setOpen(false)} />}
    </div>
    <button type="button" className="lc-inspector__collapse" data-inspector-collapse="true" aria-label={collapsed ? 'Expand inspector' : 'Collapse inspector'} aria-expanded={!collapsed} title={collapsed ? 'Expand inspector' : 'Collapse inspector'} onClick={(event) => { event.currentTarget.focus({ preventScroll: true }); toggleCollapsed(); }}><Icon name="chevron" size={17} /></button>
  </nav>;
}

function Header({ title, detail }: { title: string; detail?: string }) { return <header className="lc-inspector__header"><h2>{title}</h2>{detail && <small>{detail}</small>}</header>; }

function Histogram({ histogram, clipping, setClipping }: { histogram: unknown; clipping: boolean; setClipping: (v: boolean) => void }) {
  const data = object(histogram);
  const red = histogramBins(data.r);
  const green = histogramBins(data.g);
  const blue = histogramBins(data.b);
  const luma = histogramBins(data.luma);
  const channels = [red, green, blue, luma];
  const peak = Math.max(1, ...channels.flatMap(channel => channel));
  const path = (values: number[]) => values.map((value, index) => {
    const x = (index / (HISTOGRAM_BINS - 1)) * 100;
    const y = 100 - (value / peak) * 100;
    return `${index ? 'L' : 'M'} ${x} ${y}`;
  }).join(' ');
  const area = `${path(luma)} L 100 100 L 0 100 Z`;
  const fallbackTotal = luma.reduce((sum, value) => { const next = sum + value; return Number.isFinite(next) ? next : Number.MAX_SAFE_INTEGER; }, 0);
  const sampleCount = Math.min(Number.MAX_SAFE_INTEGER, Math.max(0, num(data.total, fallbackTotal)));
  return <div className="lc-histogram" aria-label="Histogram">
    <svg viewBox="0 0 100 100" preserveAspectRatio="none" role="img" aria-label="RGB and luminance histogram"><title>RGB and luminance histogram</title><path d="M0 100H100" stroke="currentColor" opacity=".18" fill="none" aria-hidden="true" /><path d={area} fill="#7d899d" opacity=".28" aria-hidden="true" /><path d={path(red)} stroke="#df6464" strokeWidth="1.1" vectorEffect="non-scaling-stroke" fill="none" aria-hidden="true" /><path d={path(green)} stroke="#58be86" strokeWidth="1.1" vectorEffect="non-scaling-stroke" fill="none" aria-hidden="true" /><path d={path(blue)} stroke="#6496e9" strokeWidth="1.1" vectorEffect="non-scaling-stroke" fill="none" aria-hidden="true" /><path d={path(luma)} stroke="#e0e4eb" strokeWidth="1.4" vectorEffect="non-scaling-stroke" fill="none" aria-hidden="true" /></svg>
    <div className="lc-histogram__footer"><span>{sampleCount ? `${sampleCount} samples` : 'Histogram'}</span><Checkbox className="lc-histogram__toggle" checked={clipping} onChange={setClipping} label="Clipping" /></div>
  </div>;
}

function EditPanel({ desktop }: { desktop: InspectorDesktop }) {
  const { snapshot, ui, setUi, run, histogram, setHistogram } = desktop;
  const [clipping, setClipping] = useState(ui.clipping);
  const controls = snapshot?.controls ?? [];
  const values = snapshot?.controlValues ?? {};
  const grouped = useMemo(() => {
    const map = new Map<string, ControlSpec[]>();
    for (const ctl of controls) { const key = canonicalSection(ctl.section || 'Light'); const list = map.get(key) ?? []; list.push(ctl); map.set(key, list); }
    return map;
  }, [controls]);
  const sectionOpen = (key: string) => sectionIsOpen(ui.sections, key);
  const toggleSection = (key: string) => setUi({ sections: toggledSection(ui.sections, key) });
  const settings = object(snapshot?.develop);
  const profile = object(settings.profile);
  const profileId = text(profile.id, 'lc.color');
  return <>
    <Header title="Edit" detail={snapshot?.active == null ? 'No photo' : `Photo ${snapshot.active}`} />
    <Histogram histogram={histogram} clipping={clipping} setClipping={v => { setClipping(v); setUi({ clipping: v }); }} />
    <div className="lc-inspector__actions"><button className="lc-inspector__button primary" type="button" onClick={() => void run('develop.auto', {})}>Auto</button><button className="lc-inspector__button" type="button" onClick={() => void run('develop.treatment', {})}>B&amp;W</button><button className="lc-inspector__button" type="button" onClick={() => void run('develop.reset', {})}>Reset all</button></div>
    <div className="lc-profile"><span>Profile</span><Select value={profileId} options={[{ value: profileId, label: profileId }]} onChange={id => void run('develop.profile', { id, amount: num(profile.amount, 100) })} label="Profile" /><button type="button" className="lc-inspector__button" onClick={() => setUi({ panel: 'profiles', view: 'detail' })}>Browse</button></div>
    <WhiteBalanceMode run={run} settings={settings} />
    {grouped.size === 0 && <div className="lc-inspector__empty">Controls are unavailable until active photo loads.</div>}
    {[...grouped.entries()].sort(([a], [b]) => { const ai = SECTION_ORDER.indexOf(a); const bi = SECTION_ORDER.indexOf(b); return (ai < 0 ? SECTION_ORDER.length : ai) - (bi < 0 ? SECTION_ORDER.length : bi); }).map(([key, list]) => <ControlSection key={key} name={key} controls={list} values={values} open={sectionOpen(key)} toggle={() => toggleSection(key)} run={run} settings={settings} />)}
    {grouped.has('Tone Curve') && sectionOpen('Tone Curve') && <CurveEditor run={run} settings={settings} />}
    <div className="lc-inspector__section"><button type="button" className="lc-inspector__section-head" data-open={sectionOpen('PointColor')} onClick={() => toggleSection('PointColor')}><span /><span>Point Color</span></button>{sectionOpen('PointColor') && <PointColorPanel run={run} settings={settings} />}</div>
  </>;
}

function WhiteBalanceMode({ run, settings }: { run: Run; settings: Json }) {
  const wb = object(settings.wb);
  const mode = text(wb.mode, 'custom');
  const options = ['asShot', 'auto', 'daylight', 'cloudy', 'shade', 'tungsten', 'fluorescent', 'flash', 'custom'];
  return <div className="lc-profile"><span>White balance</span><Select value={options.includes(mode) ? mode : 'custom'} options={options.map(option => ({ value: option, label: option === 'asShot' ? 'As shot' : option[0].toUpperCase() + option.slice(1) }))} onChange={value => void run('develop.wb', { mode: value })} label="White balance mode" /></div>;
}

function ControlSection({ name, controls, values, open, toggle, run, settings }: { name: string; controls: ControlSpec[]; values: Record<string, number>; open: boolean; toggle: () => void; run: Run; settings: Json }) {
  const curveSection = canonicalSection(name) === 'Tone Curve';
  const mixerSection = canonicalSection(name) === 'Color Mixer';
  return <section className="lc-inspector__section"><button type="button" className="lc-inspector__section-head" data-open={open} onClick={toggle}><span /><span>{SECTION_LABELS[name] ?? name}</span><small>{controls.length}</small></button>{open && <div className="lc-inspector__section-body">{controls.map(spec => <ControlRow key={spec.id} spec={spec} value={num(values[spec.id], spec.default)} run={run} />)}{curveSection && <div className="lc-inspector__actions"><CurvePresetPicker run={run} /><button type="button" className="lc-inspector__button" onClick={() => void run('curve.reset', { channel: 'all' })}>Reset curve</button></div>}{mixerSection && <MixerLegend settings={settings} />}</div>}</section>;
}

function CurvePresetPicker({ run }: { run: Run }) {
  const [open, setOpen] = useState(false);
  const [loading, setLoading] = useState(false);
  const [presets, setPresets] = useState<Json[]>([]);
  const [current, setCurrent] = useState<string | null>(null);
  const [error, setError] = useState('');
  const buttonRef = useRef<HTMLButtonElement>(null);
  const load = async () => {
    setOpen(true);
    setLoading(true);
    setError('');
    try {
      const result = curvePresetResult(await run('curve.presets', {}));
      setPresets(result.presets);
      setCurrent(result.current);
    } catch (reason: unknown) {
      setError(String(reason));
    } finally {
      setLoading(false);
    }
  };
  const apply = async (name: string) => {
    setCurrent(name);
    try {
      await run('curve.applyPreset', { name });
    } catch (reason: unknown) {
      setError(String(reason));
      setOpen(true);
    }
  };
  const items: MenuItem[] = loading
    ? [{ id: 'loading', label: 'Loading presets…', disabled: true }]
    : error
      ? [{ id: 'error', label: error, disabled: true }, { id: 'retry', label: 'Retry', run: () => void load() }]
      : presets.length
        ? presets.flatMap((preset): MenuItem[] => {
          const name = text(preset.name).trim();
          return name ? [{ id: name, label: name, checked: current === name, run: () => void apply(name) }] : [];
        })
        : [{ id: 'empty', label: 'No curve presets available.', disabled: true }];
  return <div className="lc-curve-picker"><button ref={buttonRef} type="button" className="lc-inspector__button" aria-haspopup="menu" aria-expanded={open} aria-busy={loading} onClick={() => void load()}>Curve presets <span aria-hidden="true">⌄</span></button>{open && buttonRef.current && <ContextMenu anchor={buttonRef.current} items={items} label="Tone curve presets" className="lc-curve-picker__menu" width={220} onClose={() => setOpen(false)} />}</div>;
}

function ControlRow({ spec, value, run }: { spec: ControlSpec; value: number; run: Run }) {
  const [draft, setDraft] = useState(value);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const started = useRef(false);
  const gestureValue = useRef(value);
  const gestureToken = useRef(0);
  const activeToken = useRef<number | null>(null);
  const pointerActive = useRef(false);
  const releasePendingToken = useRef<number | null>(null);
  const latestValue = useRef(value);
  latestValue.current = value;
  const mounted = useRef(true);
  const runRef = useRef(run);
  runRef.current = run;
  useEffect(() => { if (!started.current && releasePendingToken.current === null) setDraft(value); }, [value]);
  const trace = (phase: string, details: InteractionDiagnostic = {}) => {
    if (typeof window === 'undefined') return;
    const records = (window as DiagnosticWindow).__lcInteractionDiagnostics;
    if (!Array.isArray(records) || records.length >= 128) return;
    records.push({ at: Date.now(), control: spec.id, phase, ...details, started: started.current });
  };
  const commandQueue = useRef<SliderCommandQueue | null>(null);
  if (commandQueue.current === null) {
    commandQueue.current = new SliderCommandQueue((id, params) => {
      trace('enqueue.before', { command: id });
      const observed = runRef.current(id, params);
      return observed.then(
        result => { trace('enqueue.after', { command: id }); return result; },
        error => { trace('enqueue.error', { command: id, error: String(error) }); throw error; },
      );
    });
  }
  const enqueue = (id: string, params: JsonObject = {}) => commandQueue.current!.command(id, params);
  const release = (token: number, fallback?: number) => {
    if (releasePendingToken.current !== token) return;
    releasePendingToken.current = null;
    if (activeToken.current === null && mounted.current) setDraft(fallback ?? latestValue.current);
  };
  const failGesture = (token: number | null, reason: unknown) => {
    if (token === null || !started.current || activeToken.current !== token) return;
    trace('gesture.failed', { error: String(reason), token });
    cancel(token);
  };
  const begin = (): number | null => {
    if (started.current) { trace('begin.skipped'); return activeToken.current; }
    const token = ++gestureToken.current;
    activeToken.current = token;
    gestureValue.current = draft;
    trace('begin.before');
    started.current = true;
    trace('begin.started');
    void commandQueue.current!.begin(spec.label).catch(reason => {
      if (activeToken.current !== token) return;
      trace('begin.failed', { error: String(reason), token });
      cancel(token);
    });
    return token;
  };
  const end = (expectedToken?: number) => {
    if (expectedToken !== undefined && activeToken.current !== expectedToken) { trace('end.stale', { token: expectedToken }); return; }
    if (timer.current) { clearTimeout(timer.current); timer.current = null; }
    if (!started.current) { trace('end.skipped'); return; }
    const token = activeToken.current;
    if (token === null) return;
    trace('end.before');
    started.current = false;
    activeToken.current = null;
    releasePendingToken.current = token;
    trace('end.started');
    void commandQueue.current!.end().then(() => release(token), () => release(token));
  };
  const cancel = (expectedToken?: number) => {
    if (expectedToken !== undefined && activeToken.current !== expectedToken) { trace('cancel.stale', { token: expectedToken }); return; }
    if (timer.current) { clearTimeout(timer.current); timer.current = null; }
    if (!started.current) { trace('cancel.skipped'); return; }
    const token = activeToken.current;
    if (token === null) return;
    trace('cancel.before');
    started.current = false;
    activeToken.current = null;
    releasePendingToken.current = token;
    if (mounted.current) setDraft(gestureValue.current);
    trace('cancel.started');
    void commandQueue.current!.cancel().then(() => release(token, gestureValue.current), () => release(token, gestureValue.current));
  };
  const set = (next: number) => { if (!Number.isFinite(next)) return Promise.resolve(); const clamped = Math.max(spec.min, Math.min(spec.max, next)); if (mounted.current) setDraft(clamped); return commandQueue.current!.set(spec.id, clamped); };
  const onKey = (next: number) => { if (pointerActive.current) return; if (timer.current) { clearTimeout(timer.current); timer.current = null; } const token = begin(); if (token === null) return; void set(next).then(() => { if (!started.current || activeToken.current !== token || pointerActive.current) return; if (timer.current) clearTimeout(timer.current); timer.current = setTimeout(() => { if (!pointerActive.current) end(token); }, 400); }, reason => failGesture(token, reason)); };
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  useEffect(() => () => { if (timer.current) clearTimeout(timer.current); if (started.current) cancel(); }, [run]);
  return <div className="lc-inspector__row"><label htmlFor={`ctl-${spec.id}`}>{spec.label}</label><input id={`ctl-${spec.id}`} type="range" {...rangeProps(spec, draft)} aria-valuenow={draft} aria-valuetext={`${Number(draft.toFixed(spec.decimals))}`} aria-label={spec.label} onPointerDown={() => { pointerActive.current = true; if (timer.current) { clearTimeout(timer.current); timer.current = null; } begin(); }} onPointerUp={() => { pointerActive.current = false; end(); }} onPointerCancel={() => { pointerActive.current = false; cancel(); }} onChange={e => { const token = begin(); void set(fromRange(spec, Number(e.target.value))).catch(reason => failGesture(token, reason)); }} onKeyDown={e => { trace('keydown', { target: 'range', key: e.key, code: e.code, trusted: e.nativeEvent.isTrusted }); if (e.key === 'Escape') cancel(); else if (e.key === 'ArrowLeft' || e.key === 'ArrowDown' || e.key === 'ArrowRight' || e.key === 'ArrowUp' || e.key === 'Home' || e.key === 'End') { e.preventDefault(); const current = fromRange(spec, Number(e.currentTarget.value)); const base = Number.isFinite(current) ? current : draft; const next = e.key === 'Home' ? spec.min : e.key === 'End' ? spec.max : (e.key === 'ArrowLeft' || e.key === 'ArrowDown') ? base - spec.step : base + spec.step; onKey(next); } }} /><input type="number" min={spec.min} max={spec.max} step={spec.step} value={Number(draft.toFixed(spec.decimals))} aria-label={`${spec.label} value`} onFocus={() => begin()} onChange={e => { if (mounted.current) setDraft(Number(e.target.value)); }} onKeyDown={e => { trace('keydown', { target: 'number', key: e.key, code: e.code, trusted: e.nativeEvent.isTrusted }); if (e.key === 'Escape') cancel(); }} onBlur={() => { if (!started.current) return; const token = activeToken.current; void set(draft).then(() => end(token ?? undefined), reason => failGesture(token, reason)); }} /><button className="lc-inspector__reset" type="button" aria-label={`Reset ${spec.label}`} title={`Reset ${spec.label}`} onClick={() => { end(); void enqueue('develop.resetControl', { control: spec.id }); if (mounted.current) setDraft(spec.default); }}>↺</button></div>;
}

function MixerLegend({ settings }: { settings: Json }) { const mixer = object(settings.mixer); return <div className="lc-inspector__empty">{Object.keys(mixer).length ? 'Adjust hue, saturation & luminance for each colour.' : 'Select photo to adjust its colours.'}</div>; }

function CurveEditor({ run, settings }: { run: Run; settings: Json }) {
  const curve = object(settings.curve); const [channel, setChannel] = useState('master'); const [presets, setPresets] = useState<Json[]>([]); const [chosenPreset, setChosenPreset] = useState(''); const points = array(curve[channel]).map(point => { const value = object(point); if (Array.isArray(point)) return [num(point[0]), num(point[1])] as [number, number]; return [num(value.x), num(value.y)] as [number, number]; }).filter(([x, y]) => Number.isFinite(x) && Number.isFinite(y));
  useEffect(() => { let live = true; void run('curve.presets', {}).then((value: unknown) => { if (live) setPresets(array(object(value).presets).map(object)); }); return () => { live = false; }; }, [run]);
  const effective = points.length > 1 ? points : [[0, 0], [1, 1]] as [number, number][];
  const update = (event: React.PointerEvent<SVGSVGElement>) => { const rect = event.currentTarget.getBoundingClientRect(); const x = Math.max(0, Math.min(1, (event.clientX - rect.left) / rect.width)); const y = Math.max(0, Math.min(1, 1 - (event.clientY - rect.top) / rect.height)); const next = [...effective, [x, y] as [number, number]].sort((a, b) => a[0] - b[0]); void run('develop.curve', { channel, points: next }); };
  return <div className="lc-curve"><div className="lc-tabs">{['master', 'red', 'green', 'blue'].map(c => <button key={c} type="button" aria-selected={channel === c} onClick={() => setChannel(c)}>{c}</button>)}</div><svg viewBox="0 0 100 100" role="img" aria-label={`${channel} tone curve`} onPointerDown={update}><path d="M0 100L100 0" stroke="#535963" fill="none" strokeDasharray="2 2" /><polyline points={effective.map(([x, y]) => `${x * 100},${100 - y * 100}`).join(' ')} fill="none" stroke="var(--i-accent)" strokeWidth="1.6" vectorEffect="non-scaling-stroke" />{effective.map(([x, y], i) => <circle key={`${x}-${y}-${i}`} cx={x * 100} cy={100 - y * 100} r="2.2" fill="var(--i-accent)" />)}</svg>{presets.length > 0 && <div className="lc-inspector__row"><label>Preset</label><Select value={chosenPreset} options={[{ value: '', label: 'Choose…' }, ...presets.map(p => ({ value: text(p.name), label: text(p.name) }))]} onChange={name => { setChosenPreset(name); if (name) void run('curve.applyPreset', { name }); }} label="Preset" /></div>}<div className="lc-inspector__actions"><button type="button" className="lc-inspector__button" onClick={() => void run('file.importCurvePresets', {}).then(() => run('curve.presets', {}).then(value => setPresets(array(object(value).presets).map(object))))}>Import…</button><button type="button" className="lc-inspector__button" onClick={() => void run('file.exportCurvePresets', {})}>Export…</button></div></div>;
}

function PointColorPanel({ run, settings }: { run: Run; settings: Json }) {
  const samples = array(settings.point_colors ?? settings.pointColors); return <div>{samples.length === 0 && <div className="lc-inspector__empty">Sample a colour on stage to create point controls.</div>}{samples.map((sample, index) => <div className="lc-list__item" key={index}><strong>Sample {index + 1}</strong><button className="lc-inspector__button" type="button" onClick={() => void run('develop.resetControl', { control: `pointColor.${index}.hueShift` })}>Reset</button></div>)}</div>;
}

function ProfilesPanel({ desktop }: { desktop: InspectorDesktop }) { const { run } = desktop; const [profiles, setProfiles] = useState<Json[]>([]); const [loaded, setLoaded] = useState(false); useEffect(() => { let live = true; void run('profiles.list', {}).then((v: unknown) => { if (live) { setProfiles(array(v).map(object)); setLoaded(true); } }); return () => { live = false; }; }, [run]); const grouped = useMemo(() => { const map = new Map<string, Json[]>(); profiles.forEach(p => { const g = text(p.group, 'Profiles'); const a = map.get(g) ?? []; a.push(p); map.set(g, a); }); return map; }, [profiles]); return <><Header title="Profiles" detail={loaded ? `${profiles.length} available` : 'Loading…'} /><div className="lc-list">{[...grouped.entries()].map(([group, items]) => <section key={group}><div className="lc-inspector__empty">{group}</div>{items.map(p => <div className="lc-list__item" key={text(p.id)}><button type="button" onClick={() => void run('develop.profile', { id: text(p.id), amount: 100 })}><strong>{text(p.name, text(p.id))}</strong></button><button type="button" className="lc-inspector__profile-star" aria-label={`Favorite ${text(p.name)}`} onClick={() => void run('profile.favorite', { id: text(p.id), favorite: !Boolean(p.favorite) })}>{p.favorite ? '★' : '☆'}</button></div>)}</section>)}</div></>; }

function PresetsPanel({ desktop }: { desktop: InspectorDesktop }) { const { run } = desktop; const [presets, setPresets] = useState<Json[]>([]); const [name, setName] = useState(''); useEffect(() => { let live = true; void run('presets.list', {}).then((v: unknown) => { if (live) setPresets(array(v).map(object)); }); return () => { live = false; }; }, [run]); const groups = useMemo(() => { const map = new Map<string, Json[]>(); presets.forEach(p => { const a = map.get(text(p.group, 'Presets')) ?? []; a.push(p); map.set(text(p.group, 'Presets'), a); }); return map; }, [presets]); const refresh = async () => { const v = await run('presets.list', {}); setPresets(array(v).map(object)); }; const create = async () => { const trimmed = name.trim(); if (trimmed) { await run('preset.create', { name: trimmed }); setName(''); await refresh(); } }; return <><Header title="Presets" detail={`${presets.length} available`} /><div className="lc-list">{[...groups.entries()].map(([group, items]) => <section key={group}><div className="lc-inspector__empty">{group}</div>{items.map(p => <div className="lc-list__item" key={text(p.id)}><button type="button" onClick={() => void run('preset.apply', { id: text(p.id), amount: 100 })}><strong>{text(p.name, text(p.id))}</strong></button><button type="button" className="lc-inspector__profile-star" onClick={() => void run('preset.favorite', { id: text(p.id), favorite: !Boolean(p.favorite) })}>{p.favorite ? '★' : '☆'}</button></div>)}</section>)}</div><div className="lc-inspector__row"><label htmlFor="preset-name">Create</label><input id="preset-name" value={name} onChange={e => setName(e.target.value)} /><button type="button" className="lc-inspector__button" onClick={() => void create()}>Save</button></div><div className="lc-inspector__actions"><button type="button" className="lc-inspector__button" onClick={() => void run('file.importPresets', {}).then(refresh)}>Import…</button><button type="button" className="lc-inspector__button" onClick={() => void run('file.exportPresets', {})}>Export…</button></div></>; }

function CropPanel({ desktop }: { desktop: InspectorDesktop }) { const { run, snapshot, ui, setUi } = desktop; const controls = snapshot?.controls.filter(c => c.id === 'crop.angle' || c.section === 'Geometry') ?? []; const values = snapshot?.controlValues ?? {}; return <><Header title="Crop & Geometry" /><div className="lc-inspector__actions"><button className="lc-inspector__button" type="button" onClick={() => void run('crop.aspect', { aspect: 'free' })}>Free</button>{['1x1', '4x5', '2x3', '16x9'].map(a => <button key={a} className="lc-inspector__button" type="button" onClick={() => void run('crop.aspect', { aspect: a })}>{a}</button>)}</div><div className="lc-inspector__actions"><button className="lc-inspector__button" type="button" onClick={() => void run('crop.rotateAspect', {})}>Rotate</button><button className="lc-inspector__button" type="button" onClick={() => void run('crop.autoStraighten', {})}>Auto</button><button className="lc-inspector__button" type="button" onClick={() => void run('crop.reset', {})}>Reset</button></div>{controls.map(c => <ControlRow key={c.id} spec={c} value={num(values[c.id], c.default)} run={run} />)}<div className="lc-inspector__section"><button className="lc-inspector__section-head" type="button" data-open={true}><span /><span>Overlay</span></button><div className="lc-mask-modes">{['grid', 'thirds', 'diagonal', 'golden'].map(kind => <button type="button" key={kind} aria-pressed={ui.cropOverlay === kind} onClick={() => setUi({ cropOverlay: kind })}>{kind}</button>)}</div></div></>; }

function RemovePanel({ desktop }: { desktop: InspectorDesktop }) { const { run, snapshot, setUi } = desktop; const settings = object(snapshot?.develop); const spots = array(settings.spots); const [mode, setMode] = useState('remove'); const [size, setSize] = useState(0.04); return <><Header title="Remove" detail={`${spots.length} corrections`} /><div className="lc-inspector__actions"><button className="lc-inspector__button primary" type="button" onClick={() => void run('spot.findDust', { sensitivity: 50, add: true })}>Find Dust</button><button className="lc-inspector__button" type="button" onClick={() => void run('spot.delete', {})}>Delete Selected</button></div><div className="lc-tabs">{['remove', 'heal', 'clone'].map(m => <button key={m} type="button" aria-selected={mode === m} onClick={() => { setMode(m); setUi({ tool: m }); }}>{m}</button>)}</div><div className="lc-inspector__row"><label>Size</label><Slider value={size} min={0.001} max={0.25} step={0.001} label="Size" onChange={setSize} /><input type="number" min=".001" max=".25" step=".001" value={size} onChange={e => setSize(Number(e.target.value))} /></div><div className="lc-inspector__empty">Choose Remove, Heal, or Clone, then drag over the photo to place a correction.</div>{spots.map((spot, i) => <div className="lc-list__item" key={i}><strong>Spot {i + 1}</strong><small>{text(object(spot).mode, 'remove')}</small><button type="button" className="lc-inspector__button" onClick={() => void run('spot.select', { index: i })}>Select</button></div>)}</>; }

function MaskingPanel({ desktop }: { desktop: InspectorDesktop }) {
  const { run, snapshot, ui, setUi } = desktop;
  const settings = object(snapshot?.develop);
  const masks = array(settings.masks);
  const snapshotActive = num((snapshot as (DesktopSnapshot & { activeMask?: unknown }) | null)?.activeMask, num(object(snapshot?.source).activeMask ?? object(snapshot?.source).active_mask, 0)) || null;
  const [active, setActive] = useState<number | null>(snapshotActive);
  useEffect(() => setActive(snapshotActive), [snapshotActive]);
  const addMask = (kind: string) => {
    setUi({ tool: kind, maskOverlay: true });
    void run('mask.add', { kind });
  };
  const addComponent = (op: string) => {
    setUi({ tool: 'brush', maskOverlay: true });
    void run('mask.addComponent', { op, kind: 'brush' });
  };
  return <><Header title="Masking" detail={`${masks.length} ${masks.length === 1 ? 'mask' : 'masks'}`} />
    <div className="lc-mask-modes">{[
      ['brush', 'Brush'], ['linear', 'Linear Gradient'], ['radial', 'Radial Gradient'],
      ['colorRange', 'Color Range'], ['luminanceRange', 'Luminance Range'], ['object', 'Object (SAM)'],
      ['sky', 'Sky'], ['subject', 'Subject'], ['background', 'Background'],
    ].map(([kind, label]) => <button type="button" key={kind} aria-pressed={ui.tool === kind} onClick={() => addMask(kind)}>{label}</button>)}</div>
    <div className="lc-inspector__actions"><button className="lc-inspector__button" type="button" onClick={() => setUi({ maskOverlay: !ui.maskOverlay })}>{ui.maskOverlay ? 'Hide overlay' : 'Show overlay'}</button><button className="lc-inspector__button" type="button" onClick={() => void run('mask.deleteAll', {})}>Delete all</button></div>
    <div className="lc-inspector__actions" aria-label="Mask component mode"><button className="lc-inspector__button" type="button" onClick={() => addComponent('add')}>Add component</button><button className="lc-inspector__button" type="button" onClick={() => addComponent('subtract')}>Subtract</button><button className="lc-inspector__button" type="button" onClick={() => addComponent('intersect')}>Intersect</button></div>
    <div className="lc-inspector__row"><label>Brush size</label><Slider value={ui.brushSize} min={1} max={500} step={1} label="Brush size" onChange={brushSize => setUi({ brushSize })} /><input type="number" min="1" max="500" value={ui.brushSize} onChange={e => setUi({ brushSize: Number(e.target.value) })} /></div>
    <div className="lc-inspector__row"><label>Feather</label><Slider value={ui.brushFeather} min={0} max={100} step={1} label="Feather" onChange={brushFeather => setUi({ brushFeather })} /><input type="number" min="0" max="100" value={ui.brushFeather} onChange={e => setUi({ brushFeather: Number(e.target.value) })} /></div>
    <div className="lc-list">{masks.map((m, i) => { const mask = object(m); const id = num(mask.id, i + 1); const visible = mask.visible !== false; return <div className="lc-list__item" key={id} aria-current={active === id}><button type="button" onClick={() => { setActive(id); void run('mask.select', { id }); }}><strong>{text(mask.name, `Mask ${id}`)}</strong><small>{array(mask.components).length} component{array(mask.components).length === 1 ? '' : 's'}</small></button><button type="button" aria-label={`${visible ? 'Hide' : 'Show'} mask ${id}`} onClick={() => void run('mask.visible', { id, visible: !visible })}>{visible ? '●' : '○'}</button><button type="button" className="lc-inspector__button" onClick={() => void run('mask.invert', { id })}>Invert</button></div>; })}</div>
    {masks.length === 0 && <div className="lc-inspector__empty">Choose a mask type, then drag on photo to adjust its area.</div>}
    {ui.tool === 'colorRange' && <div className="lc-inspector__empty">Click photo to sample colour. Hold Shift to add up to five samples.</div>}
    {ui.tool === 'object' && <div className="lc-inspector__empty">Click object to include it. Hold Option to exclude a region. Requires SAM model.</div>}
  </>;
}

function RedEyePanel({ desktop }: { desktop: InspectorDesktop }) { const { run, snapshot, setUi } = desktop; const settings = object(snapshot?.develop); const eyes = array(settings.red_eye ?? settings.redEye); return <><Header title="Red Eye" detail={`${eyes.length} corrections`} /><div className="lc-inspector__actions"><button className="lc-inspector__button primary" type="button" onClick={() => { setUi({ tool: 'redeye' }); void run('redeye.add', { center: [.5, .5], rx: .08, ry: .06 }); }}>Add correction</button></div>{eyes.map((eye, i) => <div className="lc-list__item" key={i}><strong>{object(eye).pet ? 'Pet eye' : 'Red eye'} {i + 1}</strong><button type="button" className="lc-inspector__button" onClick={() => void run('redeye.delete', { index: i })}>Delete</button></div>)}{eyes.length === 0 && <div className="lc-inspector__empty">Drag over an eye in the photo to add a correction.</div>}</>; }

function VersionsPanel({ desktop }: { desktop: InspectorDesktop }) { const { run } = desktop; const [versions, setVersions] = useState<Json[]>([]); const [editing, setEditing] = useState<number | null>(null); const [name, setName] = useState(''); useEffect(() => { let live = true; void run('history.list', {}).then((v: unknown) => { if (live) setVersions(array(object(v).versions).map(object)); }); return () => { live = false; }; }, [run]); const reload = async () => { const v = await run('history.list', {}); setVersions(array(object(v).versions).map(object)); }; const rename = async (index: number) => { const trimmed = name.trim(); if (!trimmed) return; await run('version.rename', { index, name: trimmed }); setEditing(null); setName(''); await reload(); }; return <><Header title="Versions" detail={`${versions.length} saved`} /><div className="lc-inspector__actions"><button className="lc-inspector__button primary" type="button" onClick={() => void run('version.create', {}).then(reload)}>Create version</button></div><div className="lc-list">{versions.map((v, i) => <div className="lc-list__item" key={`${text(v.name)}-${i}`}>{editing === i ? <><input value={name} onChange={e => setName(e.target.value)} onKeyDown={e => { if (e.key === 'Enter') void rename(i); }} /><button type="button" className="lc-inspector__button" onClick={() => void rename(i)}>Save</button></> : <><strong>{text(v.name, `Version ${i + 1}`)}</strong><small>{text(v.created)}</small><button type="button" className="lc-inspector__button" onClick={() => void run('version.restore', { index: i })}>Restore</button><button type="button" className="lc-inspector__button" onClick={() => { setEditing(i); setName(text(v.name)); }}>Rename</button><button type="button" className="lc-inspector__button" onClick={() => void run('version.update', { index: i })}>Update</button><button type="button" className="lc-inspector__button" onClick={() => void run('version.delete', { index: i }).then(reload)}>×</button></>}</div>)}</div></>; }

function HistoryPanel({ desktop }: { desktop: InspectorDesktop }) { const { run } = desktop; const [history, setHistory] = useState<string[]>([]); useEffect(() => { let live = true; void run('history.list', {}).then((v: unknown) => { if (live) setHistory(array(object(v).history).map(x => text(x))); }); return () => { live = false; }; }, [run]); return <><Header title="History" detail={`${history.length} steps`} /><div className="lc-list">{history.map((label, i) => <button type="button" className="lc-list__item" key={`${label}-${i}`} onClick={() => void run('history.restore', { index: i })}><span className="lc-inspector__history-icon" aria-hidden="true"><Icon name="history" size={15} /></span><strong>{label || `Step ${i + 1}`}</strong><small>{i + 1}</small></button>)}</div><div className="lc-inspector__actions"><button type="button" className="lc-inspector__button" onClick={() => void run('history.clear', {})}>Clear history</button></div></>; }

function InfoPanel({ desktop }: { desktop: InspectorDesktop }) { const { snapshot, run } = desktop; const id = snapshot?.active; const [photo, setPhoto] = useState<Json>({}); const [meta, setMeta] = useState<Json>({}); const [all, setAll] = useState<Json>({}); const [metaPresets, setMetaPresets] = useState<Json[]>([]); useEffect(() => { let live = true; if (id == null) return () => { live = false; }; void run('photo.inspect', { id }).then((v: unknown) => { if (live) setPhoto(object(v)); }); void run('photo.allMetadata', { id }).then((v: unknown) => { if (live) setAll(object(v)); }); void run('metadata.presets', {}).then((v: unknown) => { if (live) setMetaPresets(array(v).map(object)); }); return () => { live = false; }; }, [id, run]); useEffect(() => { const m = object(photo.meta); setMeta(m); }, [photo]); const update = (key: string, value: string) => setMeta(current => ({ ...current, [key]: value })); const save = (key: string) => { if (id == null) return; void run('photo.setMeta', { ids: snapshot?.selection?.length ? snapshot.selection : [id], [key]: text(meta[key]) }); }; const savePreset = async () => { const name = text(meta.title).trim() || 'Metadata preset'; const value = await run('metadata.savePreset', { name }); setMetaPresets(array(value).map(object)); }; const targets = snapshot?.selection?.length ? { ids: snapshot.selection } : {}; return <><Header title="Info" detail={id == null ? 'No photo' : `Photo ${id}`} /><dl className="lc-meta"><dt>File</dt><dd>{text(photo.fileName, text(photo.file_name, '—'))}</dd><dt>Dimensions</dt><dd>{photo.width && photo.height ? `${photo.width} × ${photo.height}` : '—'}</dd><dt>Title</dt><dd><input value={text(meta.title)} onChange={e => update('title', e.target.value)} onBlur={() => save('title')} /></dd><dt>Caption</dt><dd><textarea value={text(meta.caption)} onChange={e => update('caption', e.target.value)} onBlur={() => save('caption')} /></dd><dt>Creator</dt><dd><input value={text(meta.creator)} onChange={e => update('creator', e.target.value)} onBlur={() => save('creator')} /></dd><dt>Location</dt><dd><input value={text(meta.location)} onChange={e => update('location', e.target.value)} onBlur={() => save('location')} /></dd><dt>Camera</dt><dd>{text(meta.camera, text(photo.camera, '—'))}</dd></dl><div className="lc-inspector__empty">Metadata presets</div><div className="lc-list">{metaPresets.map(p => <div className="lc-list__item" key={text(p.name)}><strong>{text(p.name)}</strong><button type="button" className="lc-inspector__button" onClick={() => void run('metadata.applyPreset', { name: text(p.name), ...targets })}>Apply</button><button type="button" className="lc-inspector__button danger" onClick={() => void run('metadata.deletePreset', { name: text(p.name) }).then(() => setMetaPresets(current => current.filter(x => text(x.name) !== text(p.name))))}>×</button></div>)}</div><div className="lc-inspector__actions"><button type="button" className="lc-inspector__button" onClick={() => void savePreset()}>Save current as preset</button></div><MetadataTable data={all} /></>; }

function MetadataTable({ data }: { data: Json }) { const rows = [...array(data.exif), ...array(data.xmp)].map(object); return <>{rows.length > 0 && <table className="lc-meta-table"><tbody>{rows.map((row, i) => <tr key={`${text(row.name, text(row.tag))}-${i}`}><td>{text(row.name, text(row.tag, text(row.group)))}</td><td>{text(row.value, '—')}</td></tr>)}</tbody></table>}</>; }

function KeywordsPanel({ desktop }: { desktop: InspectorDesktop }) { const { snapshot, run } = desktop; const id = snapshot?.active; const initial = snapshot?.selection ?? (id == null ? [] : [id]); const [keywords, setKeywords] = useState<string[]>([]); const [entry, setEntry] = useState(''); const [sets, setSets] = useState<Json[]>([]); useEffect(() => { let live = true; if (id == null) return () => { live = false; }; void run('photo.inspect', { id }).then((v: unknown) => { if (live) setKeywords(array(object(object(v).meta).keywords).map(x => text(x)).filter(Boolean)); }); void run('keyword.sets', {}).then((v: unknown) => { if (live) setSets(array(object(v).sets).map(object)); }); return () => { live = false; }; }, [id, run]); const add = () => { const value = entry.trim(); if (!value) return; const next = [...new Set([...keywords, value])]; setKeywords(next); setEntry(''); void run('photo.setMeta', { ids: initial, keywords: next }); }; const remove = (keyword: string) => { const next = keywords.filter(k => k !== keyword); setKeywords(next); void run('photo.setMeta', { ids: initial, keywords: next }); }; return <><Header title="Keywords" detail={`${keywords.length} applied`} /><div className="lc-inspector__row"><label htmlFor="keyword-entry">Add keyword</label><input id="keyword-entry" value={entry} onChange={e => setEntry(e.target.value)} onKeyDown={e => { if (e.key === 'Enter') add(); }} /><button type="button" className="lc-inspector__button" onClick={add}>Add</button></div><div className="lc-inspector__chips">{keywords.map(k => <span className="lc-inspector__chip" key={k}>{k}<button type="button" aria-label={`Remove ${k}`} onClick={() => remove(k)}>×</button></span>)}</div><div className="lc-inspector__empty">Keyword sets</div><div className="lc-list">{sets.map(set => <div className="lc-list__item" key={text(set.name)}><strong>{text(set.name)}</strong><small>{array(set.keywords).length}</small><button type="button" className="lc-inspector__button" onClick={() => void run('keyword.useSet', { name: text(set.name) })}>Use</button></div>)}</div><div className="lc-inspector__actions"><button type="button" className="lc-inspector__button" onClick={() => void run('keyword.saveSet', { name: 'Current', keywords })}>Save current set</button></div></>; }
