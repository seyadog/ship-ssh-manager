//! Data model (folders and servers) and its JSON persistence.
//! Passwords never go through here: they live in the encrypted vault (see `vault`).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Auth {
    #[default]
    Agent,
    Key,
    Password,
}

impl Auth {
    pub fn label(self) -> &'static str {
        match self {
            Auth::Agent => "SSH agent",
            Auth::Key => "Private key",
            Auth::Password => "Password",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Auth::Agent => Auth::Key,
            Auth::Key => Auth::Password,
            Auth::Password => Auth::Agent,
        }
    }

    pub fn prev(self) -> Self {
        self.next().next()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Server {
    pub id: u64,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: Auth,
    #[serde(default)]
    pub key_path: String,
    #[serde(default)]
    pub parent: Option<u64>,
    /// Server this one is reached through (bastion / jump host).
    #[serde(default)]
    pub jump: Option<u64>,
    /// A login password / key passphrase is saved in the vault (never the secret itself).
    #[serde(default)]
    pub has_secret: bool,
    /// A sudo/su password is saved in the vault.
    #[serde(default)]
    pub has_sudo: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Folder {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub parent: Option<u64>,
    /// Folders start closed, and stay as the user leaves them.
    #[serde(default)]
    pub expanded: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeId {
    Folder(u64),
    Server(u64),
    /// A space (see `spaces`). Spaces are not part of this store, so every operation here ignores them.
    Space(u64),
    /// The “bastions” section title of the SSH view. Not part of this store either.
    BastionsHeader,
}

/// Sibling order is the order of the vectors (folders first, then servers).
#[derive(Default, Serialize, Deserialize)]
pub struct Store {
    next_id: u64,
    pub folders: Vec<Folder>,
    pub servers: Vec<Server>,
    #[serde(skip)]
    path: PathBuf,
}

impl Store {
    /// Config directory: `$SHIP_CONFIG_DIR` or the system default.
    pub fn config_dir() -> PathBuf {
        if let Some(d) = std::env::var_os("SHIP_CONFIG_DIR") {
            return PathBuf::from(d);
        }
        directories::ProjectDirs::from("", "", "ship")
            .map(|d| d.config_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".ship"))
    }

    pub fn load() -> Result<Self> {
        Self::load_from(Self::config_dir().join("servers.json"))
    }

    pub fn load_from(path: PathBuf) -> Result<Self> {
        let mut store = if path.exists() {
            let text = std::fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))?;
            serde_json::from_str(&text).with_context(|| format!("{} is not valid JSON", path.display()))?
        } else {
            Store::default()
        };
        store.path = path;
        Ok(store)
    }

    /// Atomic write: temp file + rename.
    pub fn save(&self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn server(&self, id: u64) -> Option<&Server> {
        self.servers.iter().find(|s| s.id == id)
    }

    pub fn folder(&self, id: u64) -> Option<&Folder> {
        self.folders.iter().find(|f| f.id == id)
    }

    /// Hops needed to reach `id`, outermost first (excluding `id` itself).
    pub fn jump_chain(&self, id: u64) -> Vec<&Server> {
        let mut chain = vec![];
        let mut cur = self.server(id).and_then(|s| s.jump);
        while let Some(j) = cur {
            if chain.len() > 16 || j == id {
                break;
            }
            let Some(s) = self.server(j) else { break };
            chain.push(s);
            cur = s.jump;
        }
        chain.reverse();
        chain
    }

    /// Would routing `id` through `via` create a loop?
    pub fn would_cycle(&self, id: u64, via: u64) -> bool {
        id == via || self.jump_chain(via).iter().any(|s| s.id == id)
    }

    /// Route `id` through `via` (or directly, with `None`).
    pub fn move_via(&mut self, id: u64, via: Option<u64>) -> bool {
        if via.is_some_and(|v| self.would_cycle(id, v)) {
            return false;
        }
        match self.servers.iter_mut().find(|s| s.id == id) {
            Some(s) => {
                s.jump = via;
                true
            }
            None => false,
        }
    }

    pub fn add_folder(&mut self, name: String, parent: Option<u64>) -> u64 {
        let id = self.new_id();
        self.folders.push(Folder { id, name, parent, expanded: false });
        id
    }

    pub fn add_server(&mut self, mut s: Server) -> u64 {
        s.id = self.new_id();
        let id = s.id;
        self.servers.push(s);
        id
    }

    pub fn update_server(&mut self, s: Server) {
        if let Some(slot) = self.servers.iter_mut().find(|x| x.id == s.id) {
            *slot = s;
        }
    }

    pub fn parent_of(&self, node: NodeId) -> Option<u64> {
        match node {
            NodeId::Folder(id) => self.folder(id).and_then(|f| f.parent),
            NodeId::Server(id) => self.server(id).and_then(|s| s.parent),
            NodeId::Space(_) | NodeId::BastionsHeader => None,
        }
    }

    /// Is `folder` equal to `ancestor` or inside it?
    pub fn is_inside(&self, folder: Option<u64>, ancestor: u64) -> bool {
        let mut cur = folder;
        while let Some(id) = cur {
            if id == ancestor {
                return true;
            }
            cur = self.folder(id).and_then(|f| f.parent);
        }
        false
    }

    /// Deletes the node (and everything inside, for a folder).
    /// Returns the ids of the removed servers so their secrets can be cleaned up.
    pub fn delete(&mut self, node: NodeId) -> Vec<u64> {
        let gone = self.delete_nodes(node);
        for s in &mut self.servers {
            if s.jump.is_some_and(|j| gone.contains(&j)) {
                s.jump = None;
            }
        }
        gone
    }

    fn delete_nodes(&mut self, node: NodeId) -> Vec<u64> {
        match node {
            NodeId::Space(_) | NodeId::BastionsHeader => vec![],
            NodeId::Server(id) => {
                self.servers.retain(|s| s.id != id);
                vec![id]
            }
            NodeId::Folder(id) => {
                let doomed: Vec<u64> =
                    self.folders.iter().filter(|f| self.is_inside(Some(f.id), id)).map(|f| f.id).collect();
                let gone: Vec<u64> = self
                    .servers
                    .iter()
                    .filter(|s| s.parent.is_some_and(|p| doomed.contains(&p)))
                    .map(|s| s.id)
                    .collect();
                self.folders.retain(|f| !doomed.contains(&f.id));
                self.servers.retain(|s| !gone.contains(&s.id));
                gone
            }
        }
    }

    /// Moves `node` into `dest` (at the end). Returns false if the move is invalid.
    pub fn move_into(&mut self, node: NodeId, dest: Option<u64>) -> bool {
        if let NodeId::Folder(id) = node {
            if self.is_inside(dest, id) {
                return false;
            }
        }
        match node {
            NodeId::Space(_) | NodeId::BastionsHeader => return false,
            NodeId::Folder(id) => {
                let Some(pos) = self.folders.iter().position(|f| f.id == id) else { return false };
                let mut f = self.folders.remove(pos);
                f.parent = dest;
                self.folders.push(f);
            }
            NodeId::Server(id) => {
                let Some(pos) = self.servers.iter().position(|s| s.id == id) else { return false };
                let mut s = self.servers.remove(pos);
                s.parent = dest;
                self.servers.push(s);
            }
        }
        if let Some(d) = dest {
            if let Some(f) = self.folders.iter_mut().find(|f| f.id == d) {
                f.expanded = true;
            }
        }
        true
    }

    /// Moves a server right before another one (adopting its folder).
    pub fn move_server_before(&mut self, id: u64, target: u64) -> bool {
        if id == target {
            return false;
        }
        let Some(pos) = self.servers.iter().position(|s| s.id == id) else { return false };
        let mut s = self.servers.remove(pos);
        let Some(tpos) = self.servers.iter().position(|x| x.id == target) else {
            self.servers.insert(pos, s);
            return false;
        };
        s.parent = self.servers[tpos].parent;
        self.servers.insert(tpos, s);
        true
    }

    /// Moves the node up (-1) or down (+1) among siblings of the same kind.
    pub fn shift(&mut self, node: NodeId, delta: i32, by_jump: bool) -> bool {
        fn go<T>(v: &mut [T], i: usize, delta: i32, same: impl Fn(&T, &T) -> bool) -> bool {
            let step = delta.signum();
            let mut j = i as i32 + step;
            while j >= 0 && (j as usize) < v.len() {
                if same(&v[i], &v[j as usize]) {
                    v.swap(i, j as usize);
                    return true;
                }
                j += step;
            }
            false
        }
        match node {
            NodeId::Space(_) | NodeId::BastionsHeader => false,
            NodeId::Folder(id) => {
                let Some(i) = self.folders.iter().position(|f| f.id == id) else { return false };
                go(&mut self.folders, i, delta, |a, b| a.parent == b.parent)
            }
            NodeId::Server(id) => {
                let Some(i) = self.servers.iter().position(|s| s.id == id) else { return false };
                if by_jump {
                    go(&mut self.servers, i, delta, |a, b| a.jump == b.jump)
                } else {
                    go(&mut self.servers, i, delta, |a, b| a.parent == b.parent)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srv(name: &str, parent: Option<u64>) -> Server {
        Server {
            id: 0,
            name: name.into(),
            host: format!("{name}.example"),
            port: 22,
            user: "u".into(),
            auth: Auth::Agent,
            key_path: String::new(),
            parent,
            jump: None,
            has_secret: false,
            has_sudo: false,
        }
    }

    #[test]
    fn folder_cannot_move_into_itself_or_descendant() {
        let mut s = Store::default();
        let a = s.add_folder("a".into(), None);
        let b = s.add_folder("b".into(), Some(a));
        assert!(!s.move_into(NodeId::Folder(a), Some(b)));
        assert!(!s.move_into(NodeId::Folder(a), Some(a)));
        assert!(s.move_into(NodeId::Folder(b), None));
    }

    #[test]
    fn delete_folder_removes_contents() {
        let mut s = Store::default();
        let a = s.add_folder("a".into(), None);
        let b = s.add_folder("b".into(), Some(a));
        let x = s.add_server(srv("x", Some(b)));
        let y = s.add_server(srv("y", None));
        let gone = s.delete(NodeId::Folder(a));
        assert_eq!(gone, vec![x]);
        assert!(s.folders.is_empty());
        assert_eq!(s.servers.len(), 1);
        assert_eq!(s.servers[0].id, y);
    }

    #[test]
    fn manual_order_before_and_shift() {
        let mut s = Store::default();
        let a = s.add_server(srv("a", None));
        let b = s.add_server(srv("b", None));
        let c = s.add_server(srv("c", None));
        assert!(s.move_server_before(c, a));
        let order: Vec<u64> = s.servers.iter().map(|x| x.id).collect();
        assert_eq!(order, vec![c, a, b]);
        assert!(s.shift(NodeId::Server(c), 1, false));
        let order: Vec<u64> = s.servers.iter().map(|x| x.id).collect();
        assert_eq!(order, vec![a, c, b]);
    }

    #[test]
    fn jump_chain_cycles_and_delete() {
        let mut s = Store::default();
        let a = s.add_server(srv("a", None));
        let b = s.add_server(srv("b", None));
        let c = s.add_server(srv("c", None));
        assert!(s.move_via(b, Some(a)));
        assert!(s.move_via(c, Some(b)));
        let chain: Vec<u64> = s.jump_chain(c).iter().map(|x| x.id).collect();
        assert_eq!(chain, vec![a, b]);
        assert!(!s.move_via(a, Some(c)), "a -> c -> b -> a is a loop");
        assert!(!s.move_via(a, Some(a)));
        s.delete(NodeId::Server(b));
        assert_eq!(s.server(c).unwrap().jump, None);
    }

    #[test]
    fn roundtrip_save_load() {
        let dir = std::env::temp_dir().join(format!("ship-test-{}", std::process::id()));
        let path = dir.join("servers.json");
        let mut s = Store::load_from(path.clone()).unwrap();
        s.add_server(srv("a", None));
        s.save().unwrap();
        let s2 = Store::load_from(path).unwrap();
        assert_eq!(s2.servers.len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }
}
