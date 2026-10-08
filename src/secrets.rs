//! Read-only access to the system keyring, kept only to migrate passwords saved by older
//! versions into the vault (see `App::migrate_keyring`).

use anyhow::Result;
use keyring::Entry;

const SERVICE: &str = "ship";

fn entry(server_id: u64) -> Result<Entry> {
    Ok(Entry::new(SERVICE, &format!("server-{server_id}"))?)
}

/// `None` if nothing is stored or the keyring is unavailable.
pub fn get(server_id: u64) -> Option<String> {
    entry(server_id).ok()?.get_password().ok()
}

pub fn delete(server_id: u64) {
    if let Ok(e) = entry(server_id) {
        let _ = e.delete_credential();
    }
}
