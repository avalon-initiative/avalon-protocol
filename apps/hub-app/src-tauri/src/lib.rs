//! Issue #60's one native surface: on-device secure storage for the
//! session token. Everything else — the API client, the WebAuthn/signing-key
//! auth ceremony, the session store — stays in TypeScript
//! (`@avalon-initiative/protocol-sdk` plus this app's `src/api/`); nothing else
//! moves into Rust here.
//!
//! Backed by the OS keychain via the `keyring` crate (macOS Keychain,
//! Windows Credential Manager, the Secret Service D-Bus API on Linux —
//! GNOME Keyring/KWallet, not a plain file) rather than `tauri-plugin-store`
//! (a JSON file on disk, not actually keychain-backed despite the name) or
//! an unofficial `tauri-plugin-keychain` wrapper crate: a single
//! get/set/delete command for one string value doesn't need a whole
//! plugin's permission manifest and dependency surface, and `keyring`'s
//! default (`v1`) feature set already covers every target platform without
//! any native library headers at build time (its Linux backend talks to the
//! Secret Service over D-Bus via `zbus`, a pure-Rust client).

use keyring::Entry;

// Matches tauri.conf.json's `identifier` — the keychain "service" every
// entry this app stores is filed under.
const SERVICE: &str = "protocol.avalon.hubapp";

/// Abstracts the keychain lookup so the command handlers below are testable
/// without a real OS keychain (headless CI, this sandbox) or a running
/// Tauri app context — see the `tests` module's `MemoryStore`.
trait SecureStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>, String>;
    fn set(&self, key: &str, value: &str) -> Result<(), String>;
    fn delete(&self, key: &str) -> Result<(), String>;
}

struct KeyringStore;

impl SecureStore for KeyringStore {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        let entry = Entry::new(SERVICE, key).map_err(|e| e.to_string())?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        let entry = Entry::new(SERVICE, key).map_err(|e| e.to_string())?;
        entry.set_password(value).map_err(|e| e.to_string())
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        let entry = Entry::new(SERVICE, key).map_err(|e| e.to_string())?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

// Plain functions taking `&dyn SecureStore`, wrapped by the `#[tauri::command]`
// handlers below — keeps the actual logic testable without going through
// `tauri::State`/a running app (see the `tests` module).
fn get_impl(store: &dyn SecureStore, key: &str) -> Result<Option<String>, String> {
    store.get(key)
}

fn set_impl(store: &dyn SecureStore, key: &str, value: &str) -> Result<(), String> {
    store.set(key, value)
}

fn delete_impl(store: &dyn SecureStore, key: &str) -> Result<(), String> {
    store.delete(key)
}

#[tauri::command]
fn secure_storage_get(
    key: String,
    store: tauri::State<Box<dyn SecureStore>>,
) -> Result<Option<String>, String> {
    get_impl(&**store, &key)
}

#[tauri::command]
fn secure_storage_set(
    key: String,
    value: String,
    store: tauri::State<Box<dyn SecureStore>>,
) -> Result<(), String> {
    set_impl(&**store, &key, &value)
}

#[tauri::command]
fn secure_storage_delete(
    key: String,
    store: tauri::State<Box<dyn SecureStore>>,
) -> Result<(), String> {
    delete_impl(&**store, &key)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Epic #623, issue #640: cross-node login approval reached via a
        // deep link (`avalon://cross-node-login?node=...&user_code=...`
        // on desktop, per tauri.conf.json's `plugins.deep-link.desktop`
        // config) — the frontend listens for it in `main.ts`
        // (`onOpenUrl`/`getCurrent`) and routes into
        // `views/CrossNodeLogin.vue`. Registering the plugin is the only
        // native-side wiring this needs; everything else is TypeScript.
        //
        // Known real gap, not silently assumed away: desktop's own
        // `onOpenUrl` needs `tauri-plugin-single-instance` to route a
        // second-launch URL into the already-running app on Windows/Linux
        // (macOS gets it natively) — not added here, so a cold desktop
        // launch via deep link works (`getCurrent`), but re-triggering one
        // while already running does not yet on Windows/Linux. True iOS
        // Universal Links / Android App Links additionally need this
        // repo's native mobile projects initialized at all
        // (`tauri ios init`/`tauri android init`, neither run yet — see
        // `src-tauri/gen/` only having desktop/linux schemas) plus a real
        // hosted domain's `.well-known` association files — out of scope
        // here, tracked as a real follow-up rather than assumed done.
        .plugin(tauri_plugin_deep_link::init())
        .manage(Box::new(KeyringStore) as Box<dyn SecureStore>)
        .invoke_handler(tauri::generate_handler![
            secure_storage_get,
            secure_storage_set,
            secure_storage_delete
        ])
        .run(tauri::generate_context!())
        .expect("error while running avalon hub-app");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    // A throwaway in-memory store standing in for the OS keychain — real
    // keychain access needs a live Secret Service/Keychain/Credential
    // Manager session this sandbox (and most CI) doesn't have, so the round
    // trip is exercised against this temp store instead, through the exact
    // same `get_impl`/`set_impl`/`delete_impl` the real commands call.
    struct MemoryStore(Mutex<HashMap<String, String>>);

    impl MemoryStore {
        fn new() -> Self {
            Self(Mutex::new(HashMap::new()))
        }
    }

    impl SecureStore for MemoryStore {
        fn get(&self, key: &str) -> Result<Option<String>, String> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }

        fn set(&self, key: &str, value: &str) -> Result<(), String> {
            self.0
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }

        fn delete(&self, key: &str) -> Result<(), String> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
    }

    #[test]
    fn round_trips_a_value_through_get_set_delete() {
        let store = MemoryStore::new();
        const KEY: &str = "avalon:session:token";

        assert_eq!(get_impl(&store, KEY).unwrap(), None);

        set_impl(&store, KEY, "tok-123").unwrap();
        assert_eq!(get_impl(&store, KEY).unwrap(), Some("tok-123".to_string()));

        // Overwriting an existing entry replaces it rather than erroring —
        // the session store calls `login` again on every fresh login.
        set_impl(&store, KEY, "tok-456").unwrap();
        assert_eq!(get_impl(&store, KEY).unwrap(), Some("tok-456".to_string()));

        delete_impl(&store, KEY).unwrap();
        assert_eq!(get_impl(&store, KEY).unwrap(), None);
    }

    #[test]
    fn deleting_a_key_that_was_never_set_is_not_an_error() {
        let store = MemoryStore::new();
        assert!(delete_impl(&store, "avalon:session:identityId").is_ok());
    }

    #[test]
    fn distinct_keys_do_not_collide() {
        let store = MemoryStore::new();
        set_impl(&store, "avalon:session:token", "tok").unwrap();
        set_impl(&store, "avalon:session:identityId", "id-1").unwrap();

        assert_eq!(
            get_impl(&store, "avalon:session:token").unwrap(),
            Some("tok".to_string())
        );
        assert_eq!(
            get_impl(&store, "avalon:session:identityId").unwrap(),
            Some("id-1".to_string())
        );
    }
}
