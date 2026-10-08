//! Passwords and passphrases in the system keyring (Secret Service, Keychain, Credential Manager).
//! They are never written to `servers.json`.

use anyhow::Result;
use keyring::Entry;

const SERVICE: &str = "ship";

fn entry(server_id: u64) -> Result<Entry> {
    Ok(Entry::new(SERVICE, &format!("server-{server_id}"))?)
}

pub fn set(server_id: u64, secret: &str) -> Result<()> {
    entry(server_id)?.set_password(secret)?;
    Ok(())
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
