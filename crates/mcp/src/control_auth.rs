//! Authentication of the desktop app's loopback control channel (`docs/control-protocol.md`).
//!
//! Every control connection must open with one line, `{"hello":{"token":"<secret>"}}`, before any
//! request runs. The secret is random per app launch. The app writes it to an owner-only file
//! (`<config dir>/control/<port>.token`) and removes it on a clean exit; a client started by the
//! same user reads that file, or takes the secret from `LIGHTCRAFT_CONTROL_TOKEN`. This module is
//! shared by the server (`apps/lightcraft/src/control_server.rs`) and every client ([`crate::Remote`],
//! which `lightcraft-cli --connect` and the MCP connect mode use). The secret is never logged: no
//! function here puts it into an error message.

use std::path::{Path, PathBuf};

/// Environment variable a client reads the secret from (for processes that launch the client
/// themselves). It takes precedence over the token file.
pub const TOKEN_ENV: &str = "LIGHTCRAFT_CONTROL_TOKEN";
/// Shortest secret a client or server accepts (a 128-bit secret is 32 hex digits).
pub const MIN_TOKEN_LEN: usize = 32;
/// Longest secret accepted from the environment or a token file.
pub const MAX_TOKEN_LEN: usize = 256;

/// Compare `given` with the `expected` secret without stopping at the first difference: the time
/// depends only on the length of `expected`. Different lengths are unequal.
pub fn ct_eq(expected: &[u8], given: &[u8]) -> bool {
    let mut diff = expected.len() ^ given.len();
    for (i, e) in expected.iter().enumerate() {
        // A byte `given` lacks counts as 0, so a short `given` still walks every byte (the length term fails it).
        diff |= usize::from(*e ^ given.get(i).copied().unwrap_or(0));
    }
    std::hint::black_box(diff) == 0
}

/// The opening line a client sends (without the newline).
pub fn hello_line(token: &str) -> String {
    serde_json::json!({"hello": {"token": token}}).to_string()
}

/// True when `token` has the shape of a secret: long enough, bounded, printable ASCII without spaces.
pub fn plausible_token(token: &str) -> bool {
    (MIN_TOKEN_LEN..=MAX_TOKEN_LEN).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_graphic())
}

/// The token file for `port` inside the settings folder `config_dir`: `<config_dir>/control/<port>.token`.
pub fn token_path(config_dir: &Path, port: u16) -> PathBuf {
    config_dir.join("control").join(format!("{port}.token"))
}

/// The token file for `port` in Ember's settings folder, or `None` when there is none.
#[cfg(not(target_arch = "wasm32"))]
pub fn default_token_path(port: u16) -> Option<PathBuf> {
    lightcraft_engine::config::config_dir().map(|dir| token_path(&dir, port))
}

/// Write `token` to `path` for this launch: the folder is made owner-only if new, and the file is
/// replaced atomically by a new owner-only file (so a file left by an earlier launch, or one
/// someone widened, is never reused: this is the repair). The result is verified before returning;
/// a file that is not owner-only afterwards is removed and refused.
#[cfg(not(target_arch = "wasm32"))]
pub fn write_token_file(path: &Path, token: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        rightkit_fs::ensure_private_dir(dir).map_err(|e| format!("cannot prepare {}: {e}", dir.display()))?;
    }
    rightkit_fs::write_private_text(path, &format!("{token}\n")).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    if let Err(e) = rightkit_fs::private::verify_owner_only(path) {
        let _ = std::fs::remove_file(path);
        return Err(format!("{} is not owner-only after writing ({e}); removed it", path.display()));
    }
    Ok(())
}

/// Remove the token file (best effort: it may already be gone).
#[cfg(not(target_arch = "wasm32"))]
pub fn remove_token_file(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Read the secret from the token file `path`. A file that other users could read, that is owned
/// by someone else, or that is a symlink is refused, never used.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_token_file(path: &Path) -> Result<String, String> {
    use std::io::Read;
    rightkit_fs::private::verify_owner_only(path)
        .map_err(|e| format!("refusing control token file {} ({e}); it must be owner-only. Restart Ember to write a new one", path.display()))?;
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_TOKEN_LEN as u64 + 2).read_to_string(&mut text))
        .map_err(|e| format!("cannot read control token file {}: {e}", path.display()))?;
    let token = text.trim();
    if plausible_token(token) { Ok(token.to_string()) } else { Err(format!("control token file {} is malformed", path.display())) }
}

/// The secret a client uses for the app on `port`: `LIGHTCRAFT_CONTROL_TOKEN` if set, else the
/// token file. `Ok(None)` when neither exists (the app will then refuse the connection and say how
/// to authenticate); `Err` when one exists but must not be used.
pub fn client_token(port: u16) -> Result<Option<String>, String> {
    client_token_with(std::env::var(TOKEN_ENV).ok(), port)
}

fn client_token_with(env: Option<String>, port: u16) -> Result<Option<String>, String> {
    if let Some(value) = env.filter(|v| !v.trim().is_empty()) {
        let value = value.trim();
        return if plausible_token(value) { Ok(Some(value.to_string())) } else { Err(format!("{TOKEN_ENV} is not a valid control token")) };
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(path) = default_token_path(port) {
        return match std::fs::symlink_metadata(&path) {
            Ok(_) => read_token_file(&path).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read control token file {}: {e}", path.display())),
        };
    }
    let _ = port;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_compare_handles_equal_unequal_and_different_lengths() {
        let t = b"0123456789abcdef0123456789abcdef";
        assert!(ct_eq(t, t));
        assert!(ct_eq(b"", b""));
        let mut other = *t;
        other[31] ^= 1;
        assert!(!ct_eq(t, &other));
        other = *t;
        other[0] ^= 0x80;
        assert!(!ct_eq(t, &other));
        assert!(!ct_eq(t, &t[..31]));
        assert!(!ct_eq(t, b""));
        assert!(!ct_eq(&t[..31], t));
        assert!(!ct_eq(b"", t));
        let mut longer = t.to_vec();
        longer.push(b'0');
        assert!(!ct_eq(t, &longer));
    }

    #[test]
    fn token_shape_is_checked() {
        assert!(plausible_token(&"a".repeat(32)));
        assert!(!plausible_token(&"a".repeat(31)));
        assert!(!plausible_token(&"a".repeat(257)));
        assert!(!plausible_token(&format!("{} b", "a".repeat(40))));
        assert_eq!(hello_line("x"), "{\"hello\":{\"token\":\"x\"}}");
    }

    #[test]
    fn environment_token_wins_and_is_validated() {
        let t = "f".repeat(40);
        assert_eq!(client_token_with(Some(format!(" {t}\n")), 1).unwrap(), Some(t));
        assert!(client_token_with(Some("short".into()), 1).is_err());
        let e = client_token_with(Some("short".into()), 1).unwrap_err();
        assert!(!e.contains("short"), "the value must not be echoed: {e}");
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ember-ctl-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn token_file_round_trips_and_is_replaced_each_launch() {
        let dir = scratch("roundtrip");
        let path = token_path(&dir, 7980);
        assert!(path.ends_with("control/7980.token") || path.ends_with("control\\7980.token"));
        let first = "1".repeat(32);
        let second = "2".repeat(32);
        write_token_file(&path, &first).unwrap();
        assert_eq!(read_token_file(&path).unwrap(), first);
        write_token_file(&path, &second).unwrap();
        assert_eq!(read_token_file(&path).unwrap(), second);
        remove_token_file(&path);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn token_file_is_owner_only_and_a_widened_one_is_repaired_by_the_server_and_refused_by_clients() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("perms");
        let path = token_path(&dir, 7981);
        let token = "a".repeat(32);
        write_token_file(&path, &token).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()) & 0o077, 0, "the control folder is private too");

        // Someone widens it: a client refuses to use it ...
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = read_token_file(&path).unwrap_err();
        assert!(err.contains("owner-only") && !err.contains(&token), "{err}");
        // ... and the server's next write replaces it with a fresh owner-only file.
        let fresh = "b".repeat(32);
        write_token_file(&path, &fresh).unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(read_token_file(&path).unwrap(), fresh);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
