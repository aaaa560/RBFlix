use std::fs;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use crate::video::{Playlist, VideoMeta};

// Level de acesso
#[derive(Debug, Clone, PartialEq)]
pub enum AccessLevel {
    Full,
    ViewOnly,
    Fake,
    NetflixOnly,
    None,
}

// Usuário
#[derive(Debug, Clone)]
pub struct User {
    pub username: String,
    pub password: String,
    pub access: AccessLevel,
}

// Dados do usuário
#[derive(Serialize, Deserialize)]
pub struct UserData {
    pub last_watched: Vec<VideoMeta>,
    pub favorites: Vec<VideoMeta>,
    pub playlists: Vec<Playlist>,
}

impl UserData {
    pub fn load(config_dir: &PathBuf) -> Self {
        let path = config_dir.join("user_data.json");
        if let Ok(content) = fs::read_to_string(&path) {
            serde_json::from_str(&content).unwrap_or_else(|_| Self::default())
        } else {
            Self::default()
        }
    }

    pub fn save(&self, config_dir: &PathBuf) {
        let path = config_dir.join("user_data.json");
        if let Ok(json) = serde_json::to_string_pretty(&self) {
            let _ = fs::write(path, json);
        }
    }
}

impl Default for UserData {
    fn default() -> Self {
        Self {
            last_watched: Vec::new(),
            favorites: Vec::new(),
            playlists: Vec::new(),
        }
    }
}