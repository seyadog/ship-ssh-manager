//! User settings (`settings.json` in the config directory). Missing file or fields mean the defaults.

use serde::Deserialize;

#[derive(Deserialize)]
pub struct Settings {
    /// Play a sound when an AI agent finishes.
    #[serde(default = "yes")]
    pub sound: bool,
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Settings { sound: true }
    }
}

impl Settings {
    pub fn load() -> Self {
        let path = crate::store::Store::config_dir().join("settings.json");
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }
}
