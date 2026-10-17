import { invoke } from '@tauri-apps/api/core';
import { createRightKitTauriClient, standardRightKitDeviceGetOrCreate } from '@rightkit/tauri';

/**
 * Typed renderer side of the shared RightKit Tauri command surface (`@rightkit/tauri`, paired with the native
 * `rightkit-device` host in `src/rk_device.rs`). Ember is a free app: it exposes the device identity only, so the
 * licence, logs and diagnostics commands of the client are not wired to native commands and must not be called.
 */
export const rightkitNative = createRightKitTauriClient<never, string>({
  invoke: <T>(command: string, args?: Record<string, unknown>) => invoke<T>(command, args),
  deviceGetOrCreate: standardRightKitDeviceGetOrCreate,
});

let deviceId: Promise<string> | null = null;

/** This install's device id. The host creates it once and the renderer can neither set nor replace it. */
export function getDeviceId(): Promise<string> {
  deviceId ??= rightkitNative.device.getOrCreate().catch((reason: unknown) => {
    deviceId = null;
    throw reason;
  });
  return deviceId;
}
