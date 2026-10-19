import { createAutoUpdateClient, type AutoUpdateStatus, type CheckForUpdate } from '@rightkit/app-update';

/** Published Ember builds: the manual download when automatic updates are not available. */
export const RELEASES_URL = 'https://github.com/Orthic-Labs/ember/releases';

/**
 * Whether the Tauri updater is live for this build. The plugin, the suite updater public key and the
 * `createUpdaterArtifacts` bundle setting are wired; this stays false (dormant) until the RightKit
 * GitHub Releases lane publishes a signed latest.json for Orthic-Labs/ember (Orthic-Labs/rightkit#237,
 * Ember #96). While false, no endpoint is contacted, no startup check runs and Help ▸ Check for
 * Updates… reports the manual download. Flip it to true only after a release has published latest.json.
 */
export const UPDATER_PROVISIONED = false;

export const NOT_PROVISIONED_MESSAGE = `Automatic updates are not set up in this build. Download the latest release from ${RELEASES_URL}`;

/** The Tauri updater plugin check (endpoint and pubkey live in tauri.conf.json plugins.updater). */
export const pluginCheck: CheckForUpdate = async (options) => {
  const { check } = await import('@tauri-apps/plugin-updater');
  return check(options?.headers ? { headers: options.headers } : undefined);
};

/** The check used while the updater is dormant: a configuration error that carries the manual download. */
export const dormantCheck: CheckForUpdate = async () => {
  throw new Error(NOT_PROVISIONED_MESSAGE);
};

function updateClient(check: CheckForUpdate, onStatus?: (status: AutoUpdateStatus) => void) {
  return createAutoUpdateClient({
    check,
    // Public artifacts take no credentials, so a manifest header is never sent to a download origin.
    strictInstallHeaders: true,
    // Ember is one free tier: every published release is eligible, so the manifest needs no tier field.
    getTier: () => 'patch',
    isEligible: () => true,
    // Startup checks report only; an install runs when the user asks for it.
    shouldAutoInstall: () => false,
    onStatus,
  });
}

/** Startup policy: one silent check per launch, only once the updater is provisioned. */
export function startupUpdateCheck(provisioned = UPDATER_PROVISIONED, check: CheckForUpdate = pluginCheck): Promise<AutoUpdateStatus> | null {
  return provisioned ? updateClient(check).checkOnce() : null;
}

/** Help ▸ Check for Updates…: runs the check and returns its status. */
export function checkForUpdates(provisioned = UPDATER_PROVISIONED, check: CheckForUpdate = pluginCheck): Promise<AutoUpdateStatus> {
  return updateClient(provisioned ? check : dormantCheck).checkOnce();
}

/** One line for the notice bar describing an update status. */
export function updateNotice(status: AutoUpdateStatus): string {
  switch (status.kind) {
    case 'up_to_date':
      return 'Ember is up to date';
    case 'available':
      return `Ember ${status.version} is available`;
    case 'ready':
      return `Ember ${status.version} is installed; restart Ember to finish`;
    case 'staged':
      return `Ember ${status.version} is downloaded and ready to install`;
    case 'deferred':
      return `Ember ${status.version} will install later`;
    case 'error':
      return status.message.replace(/^Error: /, '');
    default:
      return 'Update check in progress';
  }
}
