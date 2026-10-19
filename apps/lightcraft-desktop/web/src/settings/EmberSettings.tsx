import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { Button, Input, Select, Toggle } from '@rightkit/react-shell/react';
import {
  SettingRow,
  SettingsGroup,
  SettingsPanel,
  mergeSections,
  standardSections,
  type SettingsContext,
  type SettingsEntry,
  type SettingsSection,
} from '@rightkit/settings-ui';
import '@rightkit/settings-ui/settings.css';
import { APP_NAME } from '../branding';
import { getDeviceId } from '../rightkitTauri';
import type { DesktopContextValue } from '../types';
import { createEmberSettingsBackend, type EmberSettingsBackend } from './emberSettingsBackend';
import './emberSettings.css';

type Rec = Record<string, unknown>;
const rec = (value: unknown): Rec => (value && typeof value === 'object' && !Array.isArray(value) ? (value as Rec) : {});
const str = (value: unknown, fallback = ''): string => (typeof value === 'string' ? value : fallback);
const num = (value: unknown, fallback = 0): number => (typeof value === 'number' && Number.isFinite(value) ? value : fallback);
const list = (value: unknown): unknown[] => (Array.isArray(value) ? value : []);
const failure = (reason: unknown): string => (reason instanceof Error ? reason.message : typeof reason === 'string' ? reason : str(rec(reason).message, 'Something went wrong.'));

type Desktop = Pick<DesktopContextValue, 'ui' | 'setUi' | 'run' | 'native' | 'snapshot'>;

/** Folder picker through the native host; resolves the first chosen path, or '' when cancelled. */
async function chooseFolder(native: Desktop['native']): Promise<string> {
  const result = await native('chooseFolder', {});
  if (typeof result === 'string') return result;
  if (Array.isArray(result)) return str(result.find((path) => typeof path === 'string'));
  return '';
}

/** Text input that saves when it loses focus or Enter is pressed, not on every key. */
function CommitText({ label, value, placeholder, onCommit }: { label: string; value: string; placeholder?: string; onCommit: (value: string) => void }) {
  const [draft, setDraft] = useState(value);
  useEffect(() => { setDraft(value); }, [value]);
  const commit = () => { if (draft !== value) onCommit(draft); };
  return <Input aria-label={label} value={draft} placeholder={placeholder} onChange={(event) => setDraft(event.target.value)} onBlur={commit} onKeyDown={(event) => { if (event.key === 'Enter') commit(); }} />;
}

function CommitNumber({ label, value, min, max, step, onCommit }: { label: string; value: number; min?: number; max?: number; step?: number; onCommit: (value: number) => void }) {
  const [draft, setDraft] = useState(String(value));
  useEffect(() => { setDraft(String(value)); }, [value]);
  const commit = () => {
    const next = Number(draft);
    if (draft.trim() !== '' && Number.isFinite(next) && next !== value) onCommit(next);
    else setDraft(String(value));
  };
  return <Input type="number" aria-label={label} value={draft} min={min} max={max} step={step} onChange={(event) => setDraft(event.target.value)} onBlur={commit} onKeyDown={(event) => { if (event.key === 'Enter') commit(); }} />;
}

function Status({ message, tone = 'normal' }: { message: string; tone?: 'normal' | 'error' }) {
  return message ? <p className={tone === 'error' ? 'lc-settings-status rk-sr__err' : 'lc-settings-status'} role={tone === 'error' ? 'alert' : 'status'}>{message}</p> : null;
}

function GeneralBody({ ctx, desktop }: { ctx: SettingsContext; desktop: Desktop }) {
  const v = ctx.values;
  const [message, setMessage] = useState('');
  const libraryPath = str(desktop.snapshot?.libraryPath);
  const openLibrary = async () => {
    setMessage('');
    try { await desktop.run('app.openLibrary', {}); } catch (reason) { setMessage(failure(reason)); }
  };
  return (
    <>
      <SettingsGroup title="Library">
        <SettingRow id="general.library" label="Library" description={libraryPath || 'In memory: nothing is saved'} keywords={['catalog', 'folder', 'open']}>
          <Button onClick={() => void openLibrary()}>Open Library…</Button>
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Startup">
        <SettingRow id="general.startupView" label="Open in" keywords={['start', 'launch', 'view']}>
          <Select label="Open in" value={str(v['general.startupView'], 'last')} options={([['last', 'Last view'], ['photoGrid', 'Photo Grid'], ['detail', 'Detail']]).map(([value, label]) => ({ value, label }))} onChange={(value) => void ctx.set('general.startupView', value)} />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Culling">
        <SettingRow id="general.autoAdvance" label="Auto Advance" description="Move to the next photo after rating or flagging." keywords={['rating', 'flag', 'cull']}>
          <Toggle label="Auto Advance" checked={v['general.autoAdvance'] === true} onChange={(value) => void ctx.set('general.autoAdvance', value)} />
        </SettingRow>
        <SettingRow id="general.confirmDelete" label="Confirm photo deletion" keywords={['delete', 'trash', 'ask']}>
          <Toggle label="Confirm photo deletion" checked={v['general.confirmDelete'] === true} onChange={(value) => void ctx.set('general.confirmDelete', value)} />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="External Editor">
        <SettingRow id="general.externalEditor" label="Application" description="Photo > Edit in External Editor renders a TIFF copy, stacks it with the original and opens it here." stacked keywords={['photoshop', 'edit in', 'tiff']}>
          <CommitText label="Application" value={str(v['general.externalEditor'])} placeholder="System default" onCommit={(value) => void ctx.set('general.externalEditor', value)} />
        </SettingRow>
      </SettingsGroup>
      <Status message={message} tone="error" />
    </>
  );
}

function InterfaceBody({ ctx }: { ctx: SettingsContext }) {
  const v = ctx.values;
  const toggle = (id: string, label: string, description?: string) => (
    <SettingRow id={id} label={label} description={description}>
      <Toggle label={label} checked={v[id] === true} onChange={(value) => void ctx.set(id, value)} />
    </SettingRow>
  );
  return (
    <>
      <SettingsGroup title="Filmstrip">
        {toggle('interface.filmstrip', 'Show filmstrip')}
        {toggle('interface.filmNames', 'Show file names')}
        {toggle('interface.filmBadges', 'Show ratings, flags & edit badges')}
      </SettingsGroup>
      <SettingsGroup title="Grid">{toggle('interface.gridInfo', 'Show grid info')}</SettingsGroup>
      <SettingsGroup title="Detail">
        {toggle('interface.navigator', 'Show Navigator while zoomed in')}
        <SettingRow id="interface.infoOverlay" label="Info overlay">
          <Select label="Info overlay" value={String(num(v['interface.infoOverlay']))} options={([['0', 'Off'], ['1', 'File & date'], ['2', 'Exposure']]).map(([value, label]) => ({ value, label }))} onChange={(value) => void ctx.set('interface.infoOverlay', Number(value))} />
        </SettingRow>
      </SettingsGroup>
    </>
  );
}

function ImportBody({ ctx, desktop }: { ctx: SettingsContext; desktop: Desktop }) {
  const v = ctx.values;
  const [presets, setPresets] = useState<Rec[]>([]);
  const [metadataPresets, setMetadataPresets] = useState<Rec[]>([]);
  const [message, setMessage] = useState('');
  useEffect(() => {
    let live = true;
    void Promise.all([desktop.run('presets.list', {}), desktop.run('metadata.presets', {})]).then(([developPresets, metadata]) => {
      if (!live) return;
      setPresets(list(developPresets).map(rec));
      setMetadataPresets(list(metadata).map(rec));
    }).catch((reason: unknown) => { if (live) setMessage(`Presets unavailable: ${failure(reason)}`); });
    return () => { live = false; };
  }, [desktop]);
  const presetChoices = (noneLabel: string, current: string): Array<readonly [string, string]> => {
    const options: Array<readonly [string, string]> = [['default', noneLabel], ...presets.filter((preset) => str(preset.id)).map((preset): readonly [string, string] => [str(preset.id), str(preset.name, str(preset.id))])];
    if (current && !options.some(([id]) => id === current)) options.push([current, `${current} (missing)`]);
    return options;
  };
  const metadataValue = str(v['import.metadataPreset'], 'none');
  const metadataChoices: Array<readonly [string, string]> = [['none', 'None'], ...metadataPresets.filter((preset) => str(preset.name)).map((preset): readonly [string, string] => [str(preset.name), str(preset.name)])];
  if (metadataValue !== 'none' && !metadataChoices.some(([name]) => name === metadataValue)) metadataChoices.push([metadataValue, `${metadataValue} (missing)`]);
  const cameras = list(v['import.cameras']).map(rec);
  const setCameras = (next: Rec[]) => void ctx.set('import.cameras', next);
  const chooseAutoFolder = async () => {
    setMessage('');
    try { const path = await chooseFolder(desktop.native); if (path) await ctx.set('import.autoFolder', path); } catch (reason) { setMessage(failure(reason)); }
  };
  const rawPreset = str(v['import.rawPreset'], 'default');
  const otherPreset = str(v['import.otherPreset'], 'default');
  return (
    <>
      <SettingsGroup title="Develop defaults">
        <SettingRow id="import.rawPreset" label="Raw photos" keywords={['preset', 'develop', 'default']}>
          <Select label="Raw photos" value={rawPreset} options={(presetChoices('Ember Default', rawPreset)).map(([value, label]) => ({ value, label }))} onChange={(value) => void ctx.set('import.rawPreset', value)} />
        </SettingRow>
        <SettingRow id="import.otherPreset" label="Non-raw photos" keywords={['preset', 'jpeg']}>
          <Select label="Non-raw photos" value={otherPreset} options={(presetChoices('None', otherPreset)).map(([value, label]) => ({ value, label }))} onChange={(value) => void ctx.set('import.otherPreset', value)} />
        </SettingRow>
        <SettingRow id="import.perCamera" label="Use camera-specific defaults" keywords={['camera', 'model']}>
          <Toggle label="Use camera-specific defaults" checked={v['import.perCamera'] === true} onChange={(value) => void ctx.set('import.perCamera', value)} />
        </SettingRow>
        {v['import.perCamera'] === true ? (
          <SettingRow id="import.cameras" label="Cameras" stacked>
            <div className="lc-settings-cameras">
              {cameras.map((camera, index) => (
                <div className="lc-settings-camera" key={index}>
                  <CommitText label={`Camera ${index + 1}`} value={str(camera.camera)} placeholder="Camera model" onCommit={(value) => setCameras(cameras.map((item, i) => (i === index ? { ...item, camera: value } : item)))} />
                  <Select label={`Preset for camera ${index + 1}`} value={str(camera.preset) || 'default'} options={(presetChoices('Ember Default', str(camera.preset))).map(([value, label]) => ({ value, label }))} onChange={(value) => setCameras(cameras.map((item, i) => (i === index ? { ...item, preset: value === 'default' ? '' : value } : item)))} />
                  <Button size="sm" onClick={() => setCameras(cameras.filter((_, i) => i !== index))}>Remove</Button>
                </div>
              ))}
              <Button size="sm" onClick={() => setCameras([...cameras, { camera: '', preset: '' }])}>Add camera</Button>
            </div>
          </SettingRow>
        ) : null}
      </SettingsGroup>
      <SettingsGroup title="Metadata">
        <SettingRow id="import.copyright" label="Default copyright" keywords={['iptc', 'rights']}>
          <CommitText label="Default copyright" value={str(v['import.copyright'])} onCommit={(value) => void ctx.set('import.copyright', value)} />
        </SettingRow>
        <SettingRow id="import.creator" label="Default creator" keywords={['author', 'photographer']}>
          <CommitText label="Default creator" value={str(v['import.creator'])} onCommit={(value) => void ctx.set('import.creator', value)} />
        </SettingRow>
        <SettingRow id="import.metadataPreset" label="Metadata preset">
          <Select label="Metadata preset" value={metadataValue} options={(metadataChoices).map(([value, label]) => ({ value, label }))} onChange={(value) => void ctx.set('import.metadataPreset', value)} />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="XMP sidecars">
        <SettingRow id="xmp.autoWrite" label="Automatically write changes into XMP sidecars" keywords={['sidecar', 'xmp']}>
          <Toggle label="Automatically write changes into XMP sidecars" checked={v['xmp.autoWrite'] === true} onChange={(value) => void ctx.set('xmp.autoWrite', value)} />
        </SettingRow>
        <SettingRow id="xmp.naming" label="Sidecar names">
          <Select label="Sidecar names" value={str(v['xmp.naming'], 'stem')} options={([['stem', 'photo.xmp'], ['full', 'photo.ext.xmp']]).map(([value, label]) => ({ value, label }))} onChange={(value) => void ctx.set('xmp.naming', value)} />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Auto Import" description="The watched folder is scanned every few seconds while Ember is open.">
        <SettingRow id="import.autoFolder" label="Watched folder" stacked keywords={['watch', 'tether', 'folder']}>
          <div className="lc-settings-inline">
            <CommitText label="Watched folder" value={str(v['import.autoFolder'])} placeholder="Off" onCommit={(value) => void ctx.set('import.autoFolder', value)} />
            <Button onClick={() => void chooseAutoFolder()}>Choose Folder…</Button>
          </div>
        </SettingRow>
        <SettingRow id="import.autoCopy" label="Copy files into the library folder">
          <Toggle label="Copy files into the library folder" checked={v['import.autoCopy'] === true} onChange={(value) => void ctx.set('import.autoCopy', value)} />
        </SettingRow>
        <SettingRow id="import.autoAlbum" label="Add to album">
          <CommitText label="Add to album" value={str(v['import.autoAlbum'])} onCommit={(value) => void ctx.set('import.autoAlbum', value)} />
        </SettingRow>
      </SettingsGroup>
      <Status message={message} tone="error" />
    </>
  );
}

function PerformanceBody({ ctx, desktop, backend }: { ctx: SettingsContext; desktop: Desktop; backend: EmberSettingsBackend }) {
  const v = ctx.values;
  const [smart, setSmart] = useState<{ path: string; count: number; bytes: number } | null>(null);
  const [smartError, setSmartError] = useState('');
  const [existing, setExisting] = useState('move');
  const [message, setMessage] = useState('');
  const [tone, setTone] = useState<'normal' | 'error'>('normal');
  const loadSmart = () => backend.smartPreviews().then((next) => { setSmart(next); setSmartError(''); }, (reason: unknown) => setSmartError(failure(reason)));
  useEffect(() => { void loadSmart(); }, []); // eslint-disable-line react-hooks/exhaustive-deps
  const act = async (work: () => Promise<unknown>, done: string) => {
    setMessage('');
    try { await work(); setTone('normal'); setMessage(done); await loadSmart(); } catch (reason) { setTone('error'); setMessage(failure(reason)); }
  };
  const chooseSmartFolder = () => act(async () => {
    const path = await chooseFolder(desktop.native);
    if (path) await desktop.run('library.smartPreviewsLocation', { path, existing });
  }, 'Smart preview folder updated.');
  return (
    <>
      <SettingsGroup title="Previews and cache">
        <SettingRow id="performance.previewEdge" label="Preview edge" description="Longest edge of standard previews, in pixels (256 to 8192)." keywords={['preview', 'size', 'resolution']}>
          <CommitNumber label="Preview edge" value={num(v['performance.previewEdge'], 2048)} min={256} max={8192} step={128} onCommit={(value) => void ctx.set('performance.previewEdge', value)} />
        </SettingRow>
        <SettingRow id="performance.cacheMb" label="Cache size (MB)" keywords={['cache', 'thumbnail', 'disk']}>
          <CommitNumber label="Cache size (MB)" value={num(v['performance.cacheMb'])} min={0} onCommit={(value) => void ctx.set('performance.cacheMb', value)} />
        </SettingRow>
        <SettingRow id="performance.forgetLocalDays" label="Forget unchanged photos after (days)" keywords={['local', 'forget']}>
          <CommitNumber label="Forget unchanged photos after (days)" value={num(v['performance.forgetLocalDays'])} min={0} onCommit={(value) => void ctx.set('performance.forgetLocalDays', value)} />
        </SettingRow>
        <SettingRow id="performance.clearCache" label="Thumbnail cache" description="Remove cached thumbnails; Ember rebuilds them as needed.">
          <Button onClick={() => void act(() => desktop.run('library.clearPreviews', {}), 'Thumbnail cache cleared.')}>Clear thumbnail cache</Button>
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Memory and graphics">
        <SettingRow id="performance.memoryMb" label="Memory budget (MB; 0 = automatic)" keywords={['ram', 'memory']}>
          <CommitNumber label="Memory budget (MB; 0 = automatic)" value={num(v['performance.memoryMb'])} min={0} onCommit={(value) => void ctx.set('performance.memoryMb', value)} />
        </SettingRow>
        <SettingRow id="performance.gpu" label="Use GPU rendering when available" keywords={['gpu', 'graphics', 'acceleration']}>
          <Toggle label="Use GPU rendering when available" checked={v['performance.gpu'] === true} onChange={(value) => void ctx.set('performance.gpu', value)} />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Smart previews" description={smart?.path ? `${smart.count} smart previews, ${Math.round(smart.bytes / 1048576)} MB` : undefined}>
        {smart?.path ? (
          <>
            <SettingRow id="performance.smartPath" label="Folder" description={smart.path} keywords={['smart preview', 'location']}>
              <div className="lc-settings-inline">
                <Button onClick={() => void chooseSmartFolder()}>Choose Folder…</Button>
                <Button onClick={() => void act(() => desktop.run('library.smartPreviewsLocation', { reset: true, existing }), 'Smart previews now use the library folder.')}>Use library folder</Button>
              </div>
            </SettingRow>
            <SettingRow id="performance.smartExisting" label="Existing previews">
              <Select label="Existing previews" value={existing} options={([['move', 'Move them'], ['leave', 'Leave them'], ['discard', 'Delete them']]).map(([value, label]) => ({ value, label }))} onChange={setExisting} />
            </SettingRow>
          </>
        ) : (
          <SettingRow id="performance.smartPath" label="Smart previews" description={smartError ? `Smart preview settings unavailable: ${smartError}` : 'Smart previews require an on-disk library.'} />
        )}
      </SettingsGroup>
      <Status message={message} tone={tone} />
    </>
  );
}

function LanguageBody({ ctx }: { ctx: SettingsContext }) {
  return (
    <SettingsGroup title="Language" description="The language is saved and applied to the shell and command labels.">
      <SettingRow id="language.locale" label="Language" keywords={['locale', 'translation']}>
        <Select label="Language" value={str(ctx.values['language.locale'], 'en')} options={([['en', 'English'], ['zh-hans', '简体中文'], ['zh-hant', '繁體中文'], ['ja', '日本語'], ['pt-br', 'Português (Brasil)']]).map(([value, label]) => ({ value, label }))} onChange={(value) => void ctx.set('language.locale', value)} />
      </SettingRow>
    </SettingsGroup>
  );
}

const ENTRIES: Record<'general' | 'interface' | 'import' | 'performance' | 'language', SettingsEntry[]> = {
  general: [
    { id: 'general.library', label: 'Library', keywords: ['catalog'] },
    { id: 'general.startupView', label: 'Open in', keywords: ['startup'] },
    { id: 'general.autoAdvance', label: 'Auto Advance' },
    { id: 'general.confirmDelete', label: 'Confirm photo deletion' },
    { id: 'general.externalEditor', label: 'External Editor Application' },
  ],
  interface: [
    { id: 'interface.filmstrip', label: 'Show filmstrip' },
    { id: 'interface.filmNames', label: 'Show file names' },
    { id: 'interface.filmBadges', label: 'Show ratings, flags & edit badges' },
    { id: 'interface.gridInfo', label: 'Show grid info' },
    { id: 'interface.navigator', label: 'Show Navigator while zoomed in' },
    { id: 'interface.infoOverlay', label: 'Info overlay' },
  ],
  import: [
    { id: 'import.rawPreset', label: 'Raw photos' },
    { id: 'import.otherPreset', label: 'Non-raw photos' },
    { id: 'import.perCamera', label: 'Use camera-specific defaults' },
    { id: 'import.copyright', label: 'Default copyright' },
    { id: 'import.creator', label: 'Default creator' },
    { id: 'import.metadataPreset', label: 'Metadata preset' },
    { id: 'xmp.autoWrite', label: 'Write changes into XMP sidecars' },
    { id: 'xmp.naming', label: 'Sidecar names' },
    { id: 'import.autoFolder', label: 'Watched folder', keywords: ['auto import'] },
  ],
  performance: [
    { id: 'performance.previewEdge', label: 'Preview edge' },
    { id: 'performance.cacheMb', label: 'Cache size (MB)' },
    { id: 'performance.forgetLocalDays', label: 'Forget unchanged photos after (days)' },
    { id: 'performance.memoryMb', label: 'Memory budget (MB; 0 = automatic)' },
    { id: 'performance.gpu', label: 'Use GPU rendering when available' },
    { id: 'performance.smartPath', label: 'Smart previews' },
  ],
  language: [{ id: 'language.locale', label: 'Language' }],
};

function sectionsFor(desktop: Desktop, backend: EmberSettingsBackend, about: { version: string; deviceId: string }): SettingsSection[] {
  const app: SettingsSection[] = [
    { id: 'general', label: 'General', group: 'General', order: 200, entries: [...ENTRIES.general], render: (ctx) => <GeneralBody ctx={ctx} desktop={desktop} /> },
    { id: 'interface', label: 'Interface', group: 'General', order: 300, entries: [...ENTRIES.interface], render: (ctx) => <InterfaceBody ctx={ctx} /> },
    { id: 'import', label: 'Import', group: 'Library', order: 400, entries: [...ENTRIES.import], render: (ctx) => <ImportBody ctx={ctx} desktop={desktop} /> },
    { id: 'performance', label: 'Performance', group: 'Library', order: 500, entries: [...ENTRIES.performance], render: (ctx) => <PerformanceBody ctx={ctx} desktop={desktop} backend={backend} /> },
    { id: 'language', label: 'Language', group: 'General', order: 600, entries: [...ENTRIES.language], render: (ctx) => <LanguageBody ctx={ctx} /> },
  ];
  const standard = standardSections({
    appearance: {},
    about: {
      appName: APP_NAME,
      version: about.version,
      links: [{ label: 'Source code', url: 'https://github.com/Orthic-Labs/ember' }],
      extra: about.deviceId ? <SettingRow label="Device ID" description="Identifies this installation. It stays on this computer."><code className="lc-settings-device">{about.deviceId}</code></SettingRow> : undefined,
    },
  });
  return mergeSections(standard, app);
}

/**
 * Ember's Settings, on the RightKit settings panel. The standard Appearance and About sections come from
 * `@rightkit/settings-ui`; the other sections hold Ember's own rows. Every change is saved as it is made.
 */
export function EmberSettings({ desktop, initialSection }: { desktop: Desktop; initialSection?: string }): ReactNode {
  // The desktop context changes identity on every state change; the panel needs one stable host so its
  // effects and the backend are not rebuilt while it is open. The host reads the latest context.
  const latest = useRef(desktop);
  latest.current = desktop;
  const host = useMemo<Desktop>(() => ({
    get ui() { return latest.current.ui; },
    get snapshot() { return latest.current.snapshot; },
    setUi: (patch) => latest.current.setUi(patch),
    run: (id, params) => latest.current.run(id, params),
    native: (action, params) => latest.current.native(action, params),
  }), []);
  const backend = useMemo(() => createEmberSettingsBackend(host), [host]);
  const [about, setAbout] = useState({ version: '', deviceId: '' });
  useEffect(() => {
    let live = true;
    void host.native('aboutInfo', {}).then((info) => { if (live) setAbout((old) => ({ ...old, version: str(rec(info).version) })); }, () => undefined);
    void getDeviceId().then((deviceId) => { if (live) setAbout((old) => ({ ...old, deviceId })); }, () => undefined);
    return () => { live = false; };
  }, [host]);
  const sections = useMemo(() => sectionsFor(host, backend, about), [host, backend, about]);
  const known = sections.some((section) => section.id === initialSection);
  return (
    <div className="lc-settings-host">
      <SettingsPanel backend={backend} sections={sections} defaultActiveId={known ? initialSection : 'general'} navHeading={false} searchMode="all-sections" highlight />
    </div>
  );
}
