//! Native device identity through `rightkit-device` (RightKit mandatory rows M1 and M12).
//!
//! One `DeviceService` owns a stable per-install UUIDv4 kept in the OS credential store through
//! `rightkit-credentials`, so it survives an app-data wipe. The renderer can only ask for it
//! (`secure_device_id_get_or_create`); it can never supply or replace it. Ember is free and
//! open source, so this is the identity half only: there is no licence record and no
//! licence command.
//!
//! The owner wires it in `native.rs` with `mod rk_device;` and
//! `rk_device::tauri_commands::secure_device_id_get_or_create` in the invoke handler.

use std::sync::OnceLock;

use rightkit_credentials::{Config, Namespace, Secret, Store, WindowsTarget};
use rightkit_device::{DeviceService, SecureStore};

/// Keychain service on macOS; the Windows target prefix is "Ember".
const KEYCHAIN_SERVICE: &str = "com.orthiclabs.ember";
const WINDOWS_PREFIX: &str = "Ember";
const DEVICE_ACCOUNT: &str = "DeviceId";

/// `rightkit-device` storage over the per-app OS credential store.
pub(crate) struct KeyringDeviceStore {
    store: Store,
}

impl KeyringDeviceStore {
    fn open() -> Result<Self, String> {
        let namespace = Namespace::new(KEYCHAIN_SERVICE)
            .map_err(|error| format!("device id namespace is invalid: {error}"))?
            .windows_target(WindowsTarget::Slash { prefix: Some(WINDOWS_PREFIX.to_string()) });
        // `EMBER_NO_KEYCHAIN` is the universal RightKit guard for unattended QA: no OS prompts.
        let config = Config::new(namespace).guard_env("EMBER_NO_KEYCHAIN");
        // Unit-test binaries never reach the OS store.
        #[cfg(test)]
        let config = config.guard_when(true, rightkit_credentials::GuardMode::Memory);
        Ok(Self { store: Store::open(config) })
    }
}

impl SecureStore<String> for KeyringDeviceStore {
    fn get(&self) -> Result<Option<String>, String> {
        match self.store.get(DEVICE_ACCOUNT) {
            Ok(Some(secret)) => {
                let id = secret.expose_str().map_err(|error| format!("stored device id is unreadable: {error}"))?.trim().to_string();
                Ok(if id.is_empty() { None } else { Some(id) })
            }
            Ok(None) => Ok(None),
            Err(error) => Err(format!("could not read device id from secure storage: {error}")),
        }
    }

    fn set(&self, id: &String) -> Result<(), String> {
        self.store.set_str(DEVICE_ACCOUNT, id).map_err(|error| format!("could not save device id to secure storage: {error}"))
    }

    fn delete(&self) -> Result<(), String> {
        self.store.delete(DEVICE_ACCOUNT).map(|_| ()).map_err(|error| format!("could not remove device id from secure storage: {error}"))
    }

    /// Native create-new: two Ember processes racing to create cannot overwrite each other.
    fn set_if_absent(&self, id: &String) -> Result<bool, String> {
        self.store
            .add_if_absent(DEVICE_ACCOUNT, &Secret::from_string(id.clone()))
            .map_err(|error| format!("could not save device id to secure storage: {error}"))
    }
}

type Service = DeviceService<KeyringDeviceStore>;

fn service() -> Result<&'static Service, String> {
    static SERVICE: OnceLock<Result<Service, String>> = OnceLock::new();
    SERVICE.get_or_init(|| KeyringDeviceStore::open().map(DeviceService::new)).as_ref().map_err(Clone::clone)
}

/// The install's device id, created natively exactly once. Blocking: the credential store can wait on the OS.
pub(crate) fn device_id() -> Result<String, String> {
    service()?.get_or_create().map_err(|error| error.message)
}

// tauri-macros 2.7.1 emits a sibling `const _: () = if false { ... unreachable!() ... }` for async
// Result commands. The branch cannot run; the allowance is scoped to this generated binding.
#[allow(clippy::unreachable)]
pub(crate) mod tauri_commands {
    /// Atomic get-or-create of the device id inside the trusted native boundary. It accepts no
    /// renderer-supplied identity, and runs off the main thread so a locked keychain cannot freeze the UI.
    #[tauri::command]
    pub(crate) async fn secure_device_id_get_or_create() -> Result<String, String> {
        tauri::async_runtime::spawn_blocking(super::device_id)
            .await
            .map_err(|error| format!("join secure_device_id_get_or_create: {error}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_is_a_stable_uuid_v4() {
        let first = device_id().expect("device id");
        let second = device_id().expect("device id");
        assert_eq!(first, second);
        assert_eq!(first.len(), 36);
        assert_eq!(first.as_bytes()[14], b'4');
    }
}
