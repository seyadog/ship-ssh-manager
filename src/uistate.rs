//! What is open and what is folded in the sidebar, kept between runs (`state.json` in the config directory).
//! Folders keep their own state in `servers.json`; this is the rest: which bastions are unfolded.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default)]
pub struct UiState {
    /// Bastions (server ids) shown unfolded in the bastions section of the SSH view.
    #[serde(default)]
    pub open_nodes: Vec<u64>,
    /// For each screen, whether it is in mosaic mode (new terminals tile by themselves) or tabs mode.
    #[serde(default)]
    pub screen_modes: Vec<bool>,
}

impl UiState {
    pub fn path() -> PathBuf {
        crate::store::Store::config_dir().join("state.json")
    }

    /// A missing or unreadable file means the defaults: everything closed.
    pub fn load() -> Self {
        std::fs::read_to_string(Self::path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    /// Atomic write: temp file + rename.
    pub fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_defaults_to_closed() {
        let dir = std::env::temp_dir().join(format!("ship-ui-{}", std::process::id()));
        let path = dir.join("state.json");
        let s = UiState { open_nodes: vec![3, 7], ..Default::default() };
        s.save_to(&path).unwrap();
        let t: UiState = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(t.open_nodes, vec![3, 7]);
        let empty: UiState = serde_json::from_str("{}").unwrap();
        assert!(empty.open_nodes.is_empty(), "nothing saved: all closed");
        let old: UiState = serde_json::from_str(r#"{"bastions_open":true,"open_nodes":[1]}"#).unwrap();
        assert_eq!(old.open_nodes, vec![1], "a file from an earlier version still loads");
        std::fs::remove_dir_all(dir).ok();
    }
}
