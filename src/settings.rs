//! User settings (`settings.json` in the config directory). Missing file or fields mean the defaults.

use serde::Deserialize;

#[derive(Deserialize)]
pub struct Settings {
    /// `"terminal"` (pastels on your terminal's own background, the default) or `"classic"` (fixed colours).
    #[serde(default = "terminal")]
    pub theme: String,
}

fn terminal() -> String {
    "terminal".into()
}

impl Default for Settings {
    fn default() -> Self {
        Settings { theme: terminal() }
    }
}

impl Settings {
    pub fn load() -> Self {
        let path = crate::store::Store::config_dir().join("settings.json");
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }
}

/// Remembers the theme, keeping the rest of the file.
pub fn save_theme(theme: &str) {
    let path = crate::store::Store::config_dir().join("settings.json");
    let mut value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    value["theme"] = serde_json::Value::String(theme.into());
    if let Ok(text) = serde_json::to_string_pretty(&value) {
        let _ = std::fs::write(path, text);
    }
}
