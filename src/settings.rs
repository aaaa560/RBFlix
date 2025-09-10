use std::collections::HashMap;
use serde::{Deserialize, Serialize};

// Configurações do app
#[derive(Serialize, Deserialize)]
pub struct AppSettings {
    pub default_quality: String,
    pub auto_play_next: bool,
    pub download_thumbnails: bool,
    pub theme: String,
    pub shortcuts: HashMap<String, String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        let mut shortcuts = HashMap::new();
        shortcuts.insert("play_pause".to_string(), "Space".to_string());
        shortcuts.insert("seek_forward".to_string(), "ArrowRight".to_string());
        shortcuts.insert("seek_backward".to_string(), "ArrowLeft".to_string());
        shortcuts.insert("fullscreen".to_string(), "F".to_string());

        Self {
            default_quality: "720p".to_string(),
            auto_play_next: false,
            download_thumbnails: true,
            theme: "Dark".to_string(),
            shortcuts,
        }
    }
}