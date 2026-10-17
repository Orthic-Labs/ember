import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react';
import { Onboarding, isLegalAccepted, onboardingStateKey, type LegalStepConfig, type PersistedOnboardingState } from '@rightkit/onboarding';
import '@rightkit/onboarding/onboarding.css';
import { useDesktop } from '../desktop';
import { createPreferenceSettingsBackend } from '../settings/emberSettingsBackend';
import { LAST_STEP_ID, emberOnboardingSteps } from './steps';

export type Startup = 'checking' | 'app' | 'setup';

export interface StartupGateProps {
  /** The legal gate: documents, agreement statement and branding (from the legal lane's modules). */
  legal: LegalStepConfig;
  /** Version the user must have accepted; a different stored version asks again. */
  requiredVersion?: string;
  /** Hash of the material legal documents; a text change without a version bump asks again. */
  requiredDocumentsHash?: string;
  children: ReactNode;
}

/**
 * What to show at launch.
 * - `app`: the agreement is accepted and setup was finished or skipped.
 * - `setup`: agreement and steps, resuming at the saved step.
 * QA: `?qaState=app` skips everything, `?qaState=onboarding` forces setup.
 */
export async function resolveStartup(
  settings: ReturnType<typeof createPreferenceSettingsBackend>,
  requiredVersion?: string,
  requiredDocumentsHash?: string,
): Promise<Exclude<Startup, 'checking'>> {
  const qa = new URLSearchParams(window.location.search).get('qaState');
  if (qa === 'app') return 'app';
  if (qa === 'onboarding') return 'setup';
  try {
    const snapshot = await settings.load();
    const state = snapshot.values[onboardingStateKey()] as Partial<PersistedOnboardingState> | undefined;
    if (!state) return 'setup';
    const accepted = isLegalAccepted({ legalAccepted: false, ...state } as PersistedOnboardingState, requiredVersion, undefined, requiredDocumentsHash);
    const finished = state.skipped === true || state.completed?.includes(LAST_STEP_ID) === true;
    return accepted && finished ? 'app' : 'setup';
  } catch {
    // Preferences unreadable: show setup, which can save and resume once they are.
    return 'setup';
  }
}

/**
 * First-run flow of `@rightkit/onboarding` in front of the workspace: the legal gate, then Ember's steps.
 * Renders nothing while the saved state is read, then the workspace or the flow. Mount it inside the desktop
 * provider, because the steps use the desktop context.
 */
export function StartupGate({ legal, requiredVersion, requiredDocumentsHash, children }: StartupGateProps) {
  const settings = useMemo(() => createPreferenceSettingsBackend(), []);
  const [startup, setStartup] = useState<Startup>('checking');
  useEffect(() => {
    let live = true;
    void resolveStartup(settings, requiredVersion, requiredDocumentsHash).then((next) => { if (live) setStartup(next); });
    return () => { live = false; };
  }, [settings, requiredVersion, requiredDocumentsHash]);
  const { native } = useDesktop();
  // The workspace reports startup success to the host; while setup is shown it never mounts, and an unreported
  // start would be read as a crashed GPU start on the next launch.
  useEffect(() => {
    if (startup === 'setup') void native('startupReady', {}).catch(() => undefined);
  }, [startup, native]);
  const finish = useCallback(() => setStartup('app'), []);
  if (startup === 'checking') return null;
  if (startup === 'app') return <>{children}</>;
  return (
    <Onboarding
      settings={settings}
      legal={legal}
      steps={emberOnboardingSteps}
      requiredVersion={requiredVersion}
      requiredDocumentsHash={requiredDocumentsHash}
      onComplete={finish}
      onSkip={finish}
    />
  );
}
