use serde::{Deserialize, Serialize};
use eframe::epaint::TextureHandle;

// Resultado de vídeo
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoResult {
    pub title: String,
    pub url: String,
    pub thumbnail: Option<String>,
    pub duration: Option<String>,
    pub site: String,
}

// Metadata de vídeo
#[derive(Serialize, Deserialize, Clone)]
pub struct VideoMeta {
    pub url: String,
    pub title: String,
    pub thumbnail: Option<String>,
    pub last_position: f32,
}

// Playlist
#[derive(Serialize, Deserialize, Clone)]
pub struct Playlist {
    pub name: String,
    pub videos: Vec<VideoMeta>,
    pub created_at: String,
}

// Thumbnail do vídeo
#[allow(dead_code)]
pub struct VideoThumbnail {
    pub texture: Option<TextureHandle>,
    pub loading: bool,
}