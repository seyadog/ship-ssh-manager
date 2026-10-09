//! Encrypted password vault.
//!
//! One file (`vault.json`) holds every saved password. The key is derived from the master
//! password with Argon2id and the data is sealed with XChaCha20-Poly1305. The master password
//! and the derived key live only in memory, and only while the vault is unlocked.

use anyhow::{Context, Result, anyhow, bail};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use zeroize::{Zeroize, Zeroizing};

/// Authenticated but not encrypted: binds the ciphertext to this file format.
// Part of the encryption: it keeps the old name (the program was called ship) so that existing vaults still open.
const AAD: &[u8] = b"ship-vault-v1";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Login password, or the passphrase of a private key.
    Login,
    /// Password for `sudo` / `su` on the remote machine.
    Sudo,
}

#[derive(Default, Serialize, Deserialize, Clone)]
struct Entry {
    login: Option<String>,
    sudo: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Data {
    entries: BTreeMap<u64, Entry>,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
struct Kdf {
    m_kib: u32,
    t: u32,
    p: u32,
}

impl Kdf {
    const DEFAULT: Kdf = Kdf { m_kib: 64 * 1024, t: 3, p: 1 };
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    kdf: Kdf,
    salt: String,
    nonce: String,
    ciphertext: String,
}

struct Unlocked {
    key: Zeroizing<[u8; 32]>,
    kdf: Kdf,
    salt: [u8; 16],
    data: Data,
    last_used: Instant,
}

impl Drop for Unlocked {
    fn drop(&mut self) {
        for e in self.data.entries.values_mut() {
            e.login.zeroize();
            e.sudo.zeroize();
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum UnlockError {
    WrongPassword,
    Other(String),
}

pub struct Vault {
    path: PathBuf,
    state: Option<Unlocked>,
}

fn derive(master: &str, salt: &[u8], kdf: Kdf) -> Result<Zeroizing<[u8; 32]>> {
    let params = Params::new(kdf.m_kib, kdf.t, kdf.p, Some(32)).map_err(|e| anyhow!("bad KDF parameters: {e}"))?;
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(master.as_bytes(), salt, key.as_mut_slice())
        .map_err(|e| anyhow!("key derivation failed: {e}"))?;
    Ok(key)
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| anyhow!("no secure randomness available: {e}"))?;
    Ok(b)
}

impl Vault {
    pub fn new(path: PathBuf) -> Self {
        Vault { path, state: None }
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    pub fn is_unlocked(&self) -> bool {
        self.state.is_some()
    }

    /// Creates an empty vault protected by `master` and leaves it unlocked.
    pub fn create(&mut self, master: &str) -> Result<()> {
        self.create_with(master, Kdf::DEFAULT)
    }

    fn create_with(&mut self, master: &str, kdf: Kdf) -> Result<()> {
        if self.exists() {
            bail!("a vault already exists");
        }
        let salt = random::<16>()?;
        let key = derive(master, &salt, kdf)?;
        self.state = Some(Unlocked { key, kdf, salt, data: Data::default(), last_used: Instant::now() });
        self.save()
    }

    pub fn unlock(&mut self, master: &str) -> Result<(), UnlockError> {
        let other = |e: anyhow::Error| UnlockError::Other(format!("{e:#}"));
        let text = std::fs::read_to_string(&self.path).map_err(|e| UnlockError::Other(e.to_string()))?;
        let file: File = serde_json::from_str(&text).map_err(|e| UnlockError::Other(format!("corrupt vault: {e}")))?;
        if file.version != 1 {
            return Err(UnlockError::Other(format!("unsupported vault version {}", file.version)));
        }
        let decode = |s: &str| B64.decode(s).map_err(|e| UnlockError::Other(format!("corrupt vault: {e}")));
        let salt: [u8; 16] = decode(&file.salt)?.try_into().map_err(|_| UnlockError::Other("corrupt vault".into()))?;
        let nonce: [u8; 24] =
            decode(&file.nonce)?.try_into().map_err(|_| UnlockError::Other("corrupt vault".into()))?;
        let ciphertext = decode(&file.ciphertext)?;

        let key = derive(master, &salt, file.kdf).map_err(other)?;
        let cipher =
            XChaCha20Poly1305::new_from_slice(key.as_slice()).map_err(|_| UnlockError::Other("bad key".into()))?;
        let plain = cipher
            .decrypt(&XNonce::from(nonce), Payload { msg: &ciphertext, aad: AAD })
            .map_err(|_| UnlockError::WrongPassword)?;
        let plain = Zeroizing::new(plain);
        let data: Data =
            serde_json::from_slice(&plain).map_err(|e| UnlockError::Other(format!("corrupt vault: {e}")))?;
        self.state = Some(Unlocked { key, kdf: file.kdf, salt, data, last_used: Instant::now() });
        Ok(())
    }

    pub fn lock(&mut self) {
        self.state = None;
    }

    /// Counts as activity, postponing the idle lock.
    pub fn touch(&mut self) {
        if let Some(st) = self.state.as_mut() {
            st.last_used = Instant::now();
        }
    }

    /// Locks the vault after `after` without use.
    pub fn lock_if_idle(&mut self, after: Duration) {
        if self.state.as_ref().is_some_and(|s| s.last_used.elapsed() > after) {
            self.lock();
        }
    }

    pub fn get(&mut self, id: u64, kind: Kind) -> Option<String> {
        let st = self.state.as_mut()?;
        st.last_used = Instant::now();
        let e = st.data.entries.get(&id)?;
        match kind {
            Kind::Login => e.login.clone(),
            Kind::Sudo => e.sudo.clone(),
        }
    }

    /// Stores (or, with `None`, removes) a secret and saves the vault.
    pub fn set(&mut self, id: u64, kind: Kind, secret: Option<&str>) -> Result<()> {
        let st = self.state.as_mut().context("the vault is locked")?;
        st.last_used = Instant::now();
        let e = st.data.entries.entry(id).or_default();
        let slot = match kind {
            Kind::Login => &mut e.login,
            Kind::Sudo => &mut e.sudo,
        };
        slot.zeroize();
        *slot = secret.map(str::to_string);
        if e.login.is_none() && e.sudo.is_none() {
            st.data.entries.remove(&id);
        }
        self.save()
    }

    pub fn remove_server(&mut self, id: u64) -> Result<()> {
        let st = self.state.as_mut().context("the vault is locked")?;
        if st.data.entries.remove(&id).is_some() { self.save() } else { Ok(()) }
    }

    /// Re-encrypts everything under a new master password (the vault must be unlocked).
    pub fn change_master(&mut self, new_master: &str) -> Result<()> {
        let st = self.state.as_mut().context("the vault is locked")?;
        let salt = random::<16>()?;
        st.key = derive(new_master, &salt, st.kdf)?;
        st.salt = salt;
        st.last_used = Instant::now();
        self.save()
    }

    fn save(&self) -> Result<()> {
        let st = self.state.as_ref().context("the vault is locked")?;
        let plain = Zeroizing::new(serde_json::to_vec(&st.data)?);
        let nonce = random::<24>()?;
        let cipher = XChaCha20Poly1305::new_from_slice(st.key.as_slice()).map_err(|_| anyhow!("bad key"))?;
        let ciphertext = cipher
            .encrypt(&XNonce::from(nonce), Payload { msg: &plain, aad: AAD })
            .map_err(|_| anyhow!("encryption failed"))?;
        let file = File {
            version: 1,
            kdf: st.kdf,
            salt: B64.encode(st.salt),
            nonce: B64.encode(nonce),
            ciphertext: B64.encode(ciphertext),
        };
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&file)?)?;
        restrict_permissions(&tmp);
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// Owner-only access (0600) on Unix; Windows relies on the user profile ACLs.
fn restrict_permissions(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cheap parameters so tests stay fast; production uses `Kdf::DEFAULT`.
    const FAST: Kdf = Kdf { m_kib: 8, t: 1, p: 1 };

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ship-vault-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("vault.json")
    }

    #[test]
    fn roundtrip_and_wrong_password() {
        let path = temp("rt");
        let mut v = Vault::new(path.clone());
        v.create_with("master-1", FAST).unwrap();
        v.set(7, Kind::Login, Some("hunter2")).unwrap();
        v.set(7, Kind::Sudo, Some("rootpw")).unwrap();
        v.lock();
        assert!(v.get(7, Kind::Login).is_none(), "locked vault must not answer");

        let mut v = Vault::new(path.clone());
        assert_eq!(v.unlock("nope"), Err(UnlockError::WrongPassword));
        v.unlock("master-1").unwrap();
        assert_eq!(v.get(7, Kind::Login).as_deref(), Some("hunter2"));
        assert_eq!(v.get(7, Kind::Sudo).as_deref(), Some("rootpw"));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn file_has_no_plaintext_and_tampering_is_detected() {
        let path = temp("tamper");
        let mut v = Vault::new(path.clone());
        v.create_with("m", FAST).unwrap();
        v.set(1, Kind::Login, Some("super-secret-value")).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("super-secret-value"));

        let mut file: File = serde_json::from_str(&raw).unwrap();
        let mut ct = B64.decode(&file.ciphertext).unwrap();
        ct[0] ^= 1;
        file.ciphertext = B64.encode(ct);
        std::fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();
        let mut v = Vault::new(path.clone());
        assert_eq!(v.unlock("m"), Err(UnlockError::WrongPassword));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn change_master_keeps_data() {
        let path = temp("chg");
        let mut v = Vault::new(path.clone());
        v.create_with("old", FAST).unwrap();
        v.set(3, Kind::Login, Some("keep-me")).unwrap();
        v.change_master("new").unwrap();
        v.lock();
        let mut v = Vault::new(path.clone());
        assert_eq!(v.unlock("old"), Err(UnlockError::WrongPassword));
        v.unlock("new").unwrap();
        assert_eq!(v.get(3, Kind::Login).as_deref(), Some("keep-me"));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn removing_last_secret_drops_the_entry() {
        let path = temp("rm");
        let mut v = Vault::new(path.clone());
        v.create_with("m", FAST).unwrap();
        v.set(5, Kind::Sudo, Some("x")).unwrap();
        v.set(5, Kind::Sudo, None).unwrap();
        assert!(v.get(5, Kind::Sudo).is_none());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn idle_lock() {
        let path = temp("idle");
        let mut v = Vault::new(path.clone());
        v.create_with("m", FAST).unwrap();
        v.lock_if_idle(Duration::from_secs(60));
        assert!(v.is_unlocked());
        v.lock_if_idle(Duration::ZERO);
        std::thread::sleep(Duration::from_millis(5));
        v.lock_if_idle(Duration::ZERO);
        assert!(!v.is_unlocked());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
