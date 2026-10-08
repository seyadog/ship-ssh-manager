//! One-shot import of SSH profiles and groups from a Tabby `config.yaml`.
//! Tabby keeps passwords in its own vault, so they are not imported.

use crate::store::{Auth, Server, Store};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct Config {
    #[serde(default)]
    profiles: Vec<Profile>,
    #[serde(default)]
    groups: Vec<Group>,
}

#[derive(Deserialize)]
struct Group {
    id: String,
    name: String,
    #[serde(rename = "parentGroupId")]
    parent: Option<String>,
}

#[derive(Deserialize)]
struct Profile {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default, rename = "type")]
    kind: String,
    group: Option<String>,
    #[serde(default)]
    options: Options,
}

#[derive(Deserialize, Default)]
struct Options {
    #[serde(default)]
    host: String,
    port: Option<u16>,
    #[serde(default)]
    user: String,
    #[serde(default)]
    auth: String,
    #[serde(default, rename = "privateKeys")]
    keys: Vec<String>,
    #[serde(rename = "jumpHost")]
    jump: Option<String>,
}

pub fn default_path() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.config_dir().join("tabby/config.yaml"))
        .unwrap_or_else(|| PathBuf::from("config.yaml"))
}

/// Merges the Tabby config into `store`. Returns (folders added, servers added, servers skipped).
pub fn import(store: &mut Store, path: &Path) -> Result<(usize, usize, usize)> {
    let text = std::fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    let cfg: Config = serde_yaml_ng::from_str(&text).context("not a valid Tabby config")?;

    // Groups -> folders, parents first; reuse a folder with the same name and parent.
    let mut folder_of: HashMap<String, u64> = HashMap::new();
    let mut added_folders = 0;
    let mut pending: Vec<&Group> = cfg.groups.iter().collect();
    while !pending.is_empty() {
        let before = pending.len();
        pending.retain(|g| {
            let parent = match &g.parent {
                Some(p) => match folder_of.get(p) {
                    Some(id) => Some(*id),
                    None if cfg.groups.iter().any(|x| &x.id == p) => return true,
                    None => None,
                },
                None => None,
            };
            let id = match store.folders.iter().find(|f| f.name == g.name && f.parent == parent) {
                Some(f) => f.id,
                None => {
                    added_folders += 1;
                    store.add_folder(g.name.clone(), parent)
                }
            };
            folder_of.insert(g.id.clone(), id);
            false
        });
        if pending.len() == before {
            break; // cycle: give up on the rest
        }
    }

    // Profiles -> servers. Jump hosts are resolved in a second pass.
    let mut server_of: HashMap<String, u64> = HashMap::new();
    let (mut added, mut skipped) = (0, 0);
    let ssh: Vec<&Profile> = cfg.profiles.iter().filter(|p| p.kind == "ssh").collect();
    for p in &ssh {
        let o = &p.options;
        let parent = p.group.as_ref().and_then(|g| folder_of.get(g)).copied();
        let port = o.port.unwrap_or(22);
        if let Some(s) = store.servers.iter().find(|s| s.name == p.name && s.host == o.host && s.port == port) {
            server_of.insert(p.id.clone(), s.id);
            skipped += 1;
            continue;
        }
        let key = o.keys.first().map(|k| k.strip_prefix("file://").unwrap_or(k).to_string());
        let auth = match (o.auth.as_str(), &key) {
            ("password", _) => Auth::Password,
            (_, Some(_)) => Auth::Key,
            _ => Auth::Agent,
        };
        let id = store.add_server(Server {
            id: 0,
            name: p.name.clone(),
            host: o.host.clone(),
            port,
            user: o.user.clone(),
            auth,
            key_path: key.unwrap_or_default(),
            parent,
            jump: None,
            has_secret: false,
            has_sudo: false,
        });
        server_of.insert(p.id.clone(), id);
        added += 1;
    }
    for p in &ssh {
        let (Some(j), Some(&id)) = (&p.options.jump, server_of.get(&p.id)) else { continue };
        if let Some(&via) = server_of.get(j) {
            store.move_via(id, Some(via));
        }
    }
    store.save()?;
    Ok((added_folders, added, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_groups_keys_and_jump_hosts() {
        let dir = std::env::temp_dir().join(format!("ship-tabby-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let yaml = dir.join("config.yaml");
        std::fs::write(
            &yaml,
            "groups:\n- {id: g1, name: Labs}\nprofiles:\n\
             - {id: a, name: bastion, type: ssh, group: g1, options: {host: 1.1.1.1, user: opc, auth: publicKey, privateKeys: ['file:///k/key']}}\n\
             - {id: b, name: inner, type: ssh, group: g1, options: {host: 10.0.0.2, auth: password, jumpHost: a}}\n\
             - {id: c, name: local, type: local}\n",
        )
        .unwrap();
        let mut s = Store::load_from(dir.join("servers.json")).unwrap();
        assert_eq!(import(&mut s, &yaml).unwrap(), (1, 2, 0));
        assert_eq!(s.servers[0].key_path, "/k/key");
        assert_eq!(s.servers[0].auth, Auth::Key);
        assert_eq!(s.servers[1].jump, Some(s.servers[0].id));
        assert_eq!(import(&mut s, &yaml).unwrap(), (0, 0, 2)); // idempotent
        std::fs::remove_dir_all(dir).ok();
    }
}
