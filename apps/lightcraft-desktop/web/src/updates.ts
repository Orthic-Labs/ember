import { createAutoUpdateClient, type AutoUpdateStatus, type CheckForUpdate } from '@rightkit/app-update';

/** Published Ember builds: the manual download when automatic updates are not available. */
export const RELEASES_URL = 'https://github.com/Orthic-Labs/ember/releases';

/**
 * Whether this build has the Tauri updater provisioned: the updater plugin, the update signing
 * public key (from the right-release signing setup) and a signed update manifest endpoint for
 * Orthic-Labs/ember. Until all three exist, no endpoint is contacted and no startup check runs.
 * Set it to true only together with a `check` that calls the Tauri updater.
 */
export const UPDATER_PROVISIONED = false;

export const NOT_PROVISIONED_MESSAGE = `Automatic updates are not set up in this build. Download the latest release from ${RELEASES_URL}`;

/** The Tauri updater check. Not provisioned yet, so it reports a configuration error. */
const check: CheckForUpdate = async () => {
  throw new Error(NOT_PROVISIONED_MESSAGE);
};

function updateClient(onStatus?: (status: AutoUpdateStatus) => void) {
  return createAutoUpdateClient({
    check,
    // Public artifacts take no credentials, so a manifest header is never sent to a download origin.
    strictInstallHeaders: true,
    // Startup checks report only; an install runs when the user asks for it.
    shouldAutoInstall: () => false,
    onStatus,
  });
}

/** Startup policy: one silent check per launch, only once the updater is provisioned. */
export function startupUpdateCheck(): Promise<AutoUpdateStatus> | null {
  return UPDATER_PROVISIONED ? updateClient().checkOnce() : null;
}

/** Help ▸ Check for Updates…: runs the check and returns its status. */
export function checkForUpdates(): Promise<AutoUpdateStatus> {
  return updateClient().checkOnce();
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
    case 'error':
      return status.message.replace(/^Error: /, '');
    default:
      return 'Update check in progress';
  }
}
