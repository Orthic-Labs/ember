// Local structured logging for the web layer, on `@rightkit/logs` (M15). Events are batched and written by the
// desktop host into the app's local event log (`rk_logs.rs`). Nothing is uploaded: the user collects the
// diagnostics bundle and decides what to share. Only the fields listed in `allowedFields` reach disk.
import { createLogsClient, type LogsInvoke } from '@rightkit/logs';
import { invoke } from '@tauri-apps/api/core';

export const logs = createLogsClient({
  invoke: invoke as LogsInvoke,
  allowedFields: ['action', 'code', 'count', 'duration_ms', 'outcome', 'state', 'status', 'surface'],
});

/**
 * Record uncaught errors and unhandled promise rejections as event names (their messages are dropped by the
 * owner's privacy policy, so only the fact and the surface are kept), and flush the batch when the page hides.
 * Call once at startup.
 */
export function installWebErrorLogging(target: Window = window): void {
  target.addEventListener('error', () => {
    logs.error('web_uncaught_error', { surface: 'web' });
  });
  target.addEventListener('unhandledrejection', () => {
    logs.error('web_unhandled_rejection', { surface: 'web' });
  });
  target.addEventListener('pagehide', () => {
    void logs.flush();
  });
}
