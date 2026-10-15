import { getPreferences } from './api';

export type ThemeChoice = 'light' | 'dark' | 'system';

const BOOT_TIMEOUT_MS = 750;
let choice: ThemeChoice = 'system';

/** Theme saved in host preferences, as read before the first render. */
export function bootTheme(): ThemeChoice {
  return choice;
}

/**
 * Reads the saved theme and sets `data-theme` before React renders, so a restart in dark mode
 * never paints light first. Bounded and best effort: on a slow or failed read the app starts
 * with the system theme and hydrates the saved one as before.
 */
export async function applyBootTheme(): Promise<void> {
  try {
    const timeout = new Promise<null>((resolve) => { window.setTimeout(() => resolve(null), BOOT_TIMEOUT_MS); });
    const preferences = await Promise.race([getPreferences(), timeout]);
    const ui = preferences?.ui;
    const saved = ui && typeof ui === 'object' && !Array.isArray(ui) ? (ui as Record<string, unknown>).theme : undefined;
    if (saved === 'light' || saved === 'dark' || saved === 'system') choice = saved;
  } catch {
    // Preferences unavailable: keep the system theme.
  }
  const dark = choice === 'dark' || (choice === 'system' && window.matchMedia?.('(prefers-color-scheme: dark)').matches === true);
  document.documentElement.setAttribute('data-theme', dark ? 'dark' : 'light');
}
