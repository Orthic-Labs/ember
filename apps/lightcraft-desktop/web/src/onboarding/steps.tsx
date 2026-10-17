import { useState } from 'react';
import { Button, SegmentedControl } from '@rightkit/react-shell/react';
import type { OnboardingStep } from '@rightkit/onboarding';
import { useDesktop } from '../desktop';

/** Id of the final setup step; completing it, or choosing "Set up later", ends first-run setup. */
export const LAST_STEP_ID = 'appearance';

const failure = (reason: unknown): string => (reason instanceof Error ? reason.message : typeof reason === 'string' ? reason : 'Something went wrong.');

function LibraryStep() {
  const desktop = useDesktop();
  const [error, setError] = useState('');
  const libraryPath = typeof desktop.snapshot?.libraryPath === 'string' ? desktop.snapshot.libraryPath : '';
  const open = async () => {
    setError('');
    try { await desktop.run('app.openLibrary', {}); } catch (reason) { setError(failure(reason)); }
  };
  return (
    <>
      <p>{libraryPath ? <>Your photos are catalogued in <code className="selectable">{libraryPath}</code>.</> : 'Ember keeps the library in memory until you open or create one: nothing is saved yet.'}</p>
      <p>Ember never moves or changes your original photos. Edits are stored in the library and can be written to XMP sidecars from Settings.</p>
      <Button onClick={() => void open()}>{libraryPath ? 'Open another library…' : 'Open or create a library…'}</Button>
      {error ? <p role="alert" className="rk-sr__err">{error}</p> : null}
    </>
  );
}

function AppearanceStep() {
  const desktop = useDesktop();
  const choose = (theme: 'system' | 'light' | 'dark') => {
    void desktop.native('preferences.patch', { ui: { theme } }).then(() => desktop.setUi({ theme }), () => desktop.setUi({ theme }));
  };
  return (
    <>
      <p>Choose how Ember looks. You can change this later in Settings.</p>
      <SegmentedControl
        label="Theme"
        value={desktop.ui.theme}
        options={[{ value: 'system', label: 'System' }, { value: 'light', label: 'Light' }, { value: 'dark', label: 'Dark' }]}
        onChange={choose}
      />
    </>
  );
}

/**
 * Ember's own first-run steps, after the RightKit legal gate. There is no models step: Ember's one downloadable
 * model (segmentation) has its own consent dialog the first time it is used.
 */
export const emberOnboardingSteps: readonly OnboardingStep[] = [
  {
    id: 'library',
    label: 'Library',
    title: 'Your library',
    description: 'Where Ember keeps your catalog and edits.',
    skippable: true,
    render: () => <LibraryStep />,
  },
  {
    id: LAST_STEP_ID,
    label: 'Appearance',
    title: 'Appearance',
    description: 'Light, dark, or follow the system.',
    skippable: true,
    render: () => <AppearanceStep />,
  },
];
