use eframe::egui;
use egui::{ColorImage, Image, TextureHandle, Vec2};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use reqwest;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::sync::Arc;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;
use std::thread;

enum AsyncMessage {
    SearchComplete(Result<Vec<VideoResult>, String>),
    ThumbnailLoaded(String, Result<Vec<u8>, String>),
    RecommendationsComplete(Result<Vec<VideoResult>, String>),
}

#[derive(Debug, Clone)]
enum AccessLevel {
    Full,
    ViewOnly,
    Fake,
    NetflixOnly,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VideoResult {
    title: String,
    url: String,
    thumbnail: Option<String>,
    duration: Option<String>,
    site: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct VideoMeta {
    url: String,
    title: String,
    thumbnail: Option<String>,
    last_position: f32,
}

#[derive(Serialize, Deserialize, Clone)]
struct Playlist {
    name: String,
    videos: Vec<VideoMeta>,
    created_at: String,
}

#[derive(Serialize, Deserialize)]
struct UserData {
    last_watched: Vec<VideoMeta>,
    favorites: Vec<VideoMeta>,
    playlists: Vec<Playlist>,
}

impl UserData {
    fn load(config_dir: &PathBuf) -> Self {
        let path = config_dir.join("user_data.json");
        if let Ok(content) = fs::read_to_string(&path) {
            serde_json::from_str(&content).unwrap_or(Self::default())
        } else {
            Self::default()
        }
    }

    fn save(&self, config_dir: &PathBuf) {
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

#[derive(Serialize, Deserialize)]
struct AppSettings {
    default_quality: String,
    auto_play_next: bool,
    download_thumbnails: bool,
    theme: String,
    shortcuts: HashMap<String, String>,
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

#[derive(Debug, Clone)]
struct SiteConfig {
    base_url: String,
    search_path: String,
    video_selector: String,
    title_selector: String,
    thumbnail_selector: Option<String>,
    duration_selector: Option<String>,
    url_transform: Option<fn(&str) -> String>,
    recommendations_path: Option<String>,
    recommendations_selector: Option<String>,
}

struct WebScraper {
    client: reqwest::Client,
    site_configs: HashMap<String, SiteConfig>,
}

#[derive(Debug, Clone)]
struct User {
    username: String,
    password: String,
    access: AccessLevel,
}

#[allow(dead_code)]
struct VideoThumbnail {
    texture: Option<TextureHandle>,
    loading: bool,
}

#[derive(PartialEq)]
enum AppState {
    Login,
    MainMenu,
    VideoSearch,
    VideoResults,
    Netflix,
    History,
    PlayingVideo,
    PlaylistView,
    Settings,
    Downloads,
}

// Mensagem para comunicar entre as threads de vídeo
#[derive(Debug)]
enum VideoMessage {
    Frame(Vec<u8>, (usize, usize)),
    SetSpeed(f64),
    Play,
    Pause,
    Seek(f64),
}

struct RambleyFlixApp {
    scraper: Arc<Mutex<WebScraper>>,
    users: HashMap<String, User>,
    current_user: Option<User>,
    login_attempts: u32,
    max_attempts: u32,
    config_dir: PathBuf,

    state: AppState,
    username_input: String,
    password_input: String,
    search_input: String,
    selected_site: String,

    video_results: Vec<VideoResult>,
    video_thumbnails: HashMap<String, VideoThumbnail>,
    loading_search: bool,
    error_message: String,

    pagina_atual: usize,
    videos_por_pagina: usize,

    rt: Arc<Runtime>,

    async_sender: mpsc::Sender<AsyncMessage>,
    async_receiver: mpsc::Receiver<AsyncMessage>,

    video_texture: Option<(egui::TextureHandle, (usize, usize))>,
    video_message_receiver: Option<mpsc::Receiver<VideoMessage>>,

    current_playing_video: Option<VideoResult>,
    video_recommendations: Vec<VideoResult>,
    loading_recommendations: bool,
    recommendations_error: String,

    // Novos campos para funcionalidades melhoradas
    video_speed: f64,
    is_playing: bool,
    video_progress: f32,
    volume: f32,
    fullscreen_mode: bool,
    theater_mode: bool,

    // Sistema de playlists
    user_playlists: Vec<Playlist>,
    current_playlist: Option<Playlist>,
    show_create_playlist_dialog: bool,
    new_playlist_name: String,

    // Configurações e filtros
    app_settings: AppSettings,
    duration_filter: String,
    sort_by: String,

    // Downloads
    downloads_progress: HashMap<String, f32>,

    // Sistema de notas
    video_notes: HashMap<String, String>,

    video_speed_tx: mpsc::Sender<VideoMessage>,
    video_speed_rx: Option<mpsc::Receiver<VideoMessage>>,
}

impl WebScraper {
    fn new() -> Self {
        let client_builder = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
            .timeout(std::time::Duration::from_secs(30));

        let client = client_builder.build().unwrap();
        let mut site_configs = HashMap::new();

        site_configs.insert(
            "pornhub".to_string(),
            SiteConfig {
                base_url: "https://www.pornhub.com".to_string(),
                search_path: "/video/search?search=".to_string(),
                video_selector: "a.linkVideoThumb".to_string(),
                title_selector: "a.linkVideoThumb".to_string(),
                thumbnail_selector: Some(".phimage img".to_string()),
                duration_selector: Some(".duration".to_string()),
                url_transform: Some(|href: &str| {
                    if href.starts_with("/") {
                        format!("https://www.pornhub.com{}", href)
                    } else {
                        href.to_string()
                    }
                }),
                recommendations_path: None,
                recommendations_selector: Some("div.relatedVideos a.linkVideoThumb".to_string()),
            },
        );

        site_configs.insert(
            "youtube".to_string(),
            SiteConfig {
                base_url: "https://www.youtube.com".to_string(),
                search_path: "/results?search_query=".to_string(),
                video_selector: "ytd-video-renderer a#thumbnail".to_string(),
                title_selector: "h3.ytd-video-renderer a#video-title".to_string(),
                thumbnail_selector: Some("img.yt-core-image".to_string()),
                duration_selector: Some(
                    "span.ytd-thumbnail-overlay-time-status-renderer".to_string(),
                ),
                url_transform: Some(|href: &str| {
                    if href.starts_with("/") {
                        format!("https://www.youtube.com{}", href)
                    } else {
                        href.to_string()
                    }
                }),
                recommendations_path: None,
                recommendations_selector: Some(
                    "ytd-compact-video-renderer a#thumbnail".to_string(),
                ),
            },
        );

        Self {
            client,
            site_configs,
        }
    }

    async fn search_videos(
        &self,
        site: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<VideoResult>, String> {
        let site_key = site.to_lowercase();
        let config = self
            .site_configs
            .get(&site_key)
            .ok_or_else(|| format!("Site {} não configurado", site))?;

        let search_url = format!(
            "{}{}{}",
            config.base_url,
            config.search_path,
            query.replace(' ', "+")
        );

        println!("[DEBUG] Buscando na URL: {}", search_url);

        let html_content = self.fetch_with_retry(&search_url, 3).await?;

        let document = Html::parse_document(&html_content);
        let video_selector = Selector::parse(&config.video_selector)
            .map_err(|e| format!("Erro no seletor CSS: {:?}", e))?;

        let elements = document.select(&video_selector);
        println!(
            "[DEBUG] Seletor de vídeo '{}' encontrou {} elementos.",
            config.video_selector,
            elements.count()
        );

        let title_selector = Selector::parse(&config.title_selector)
            .map_err(|e| format!("Erro no seletor de título: {:?}", e))?;

        let mut results = Vec::new();

        for (i, element) in document.select(&video_selector).enumerate() {
            if i >= limit {
                break;
            }

            let href = element.value().attr("href").unwrap_or("");
            let url = if let Some(transform) = config.url_transform {
                transform(href)
            } else {
                href.to_string()
            };

            let title_element = document.select(&title_selector).nth(i);
            let title = if let Some(title_elem) = title_element {
                if let Some(data_title) = title_elem.value().attr("data-title") {
                    data_title.trim().to_string()
                } else {
                    title_elem.text().collect::<String>().trim().to_string()
                }
            } else {
                element.text().collect::<String>().trim().to_string()
            };

            let thumbnail = if let Some(thumb_sel) = &config.thumbnail_selector {
                if let Ok(selector) = Selector::parse(thumb_sel) {
                    element
                        .select(&selector)
                        .next()
                        .and_then(|img| img.value().attr("src"))
                        .map(|s| s.to_string())
                } else {
                    None
                }
            } else {
                None
            };

            let duration = if let Some(dur_sel) = &config.duration_selector {
                if let Ok(selector) = Selector::parse(dur_sel) {
                    element
                        .select(&selector)
                        .next()
                        .map(|elem| elem.text().collect::<String>().trim().to_string())
                } else {
                    None
                }
            } else {
                None
            };

            if !url.is_empty() && !title.is_empty() {
                results.push(VideoResult {
                    title: self.clean_title(&title),
                    url,
                    thumbnail,
                    duration,
                    site: site.to_string(),
                });
            }
        }

        Ok(results)
    }

    async fn search_recommendations(
        &self,
        video: &VideoResult,
    ) -> Result<Vec<VideoResult>, String> {
        let site_key = video.site.to_lowercase();
        let config = self
            .site_configs
            .get(&site_key)
            .ok_or_else(|| "Site não configurado".to_string())?;

        let html_content = self.fetch_with_retry(&video.url, 3).await?;
        let document = Html::parse_document(&html_content);

        let mut results = Vec::new();
        if let Some(recs_sel) = &config.recommendations_selector {
            let recs_selector = Selector::parse(recs_sel)
                .map_err(|e| format!("Erro no seletor de recomendações: {:?}", e))?;

            for element in document.select(&recs_selector) {
                let href = element.value().attr("href").unwrap_or("");
                let url = if let Some(transform) = config.url_transform {
                    transform(href)
                } else {
                    href.to_string()
                };

                let title_selector = Selector::parse(&config.title_selector).ok();
                let title = if let Some(title_sel) = title_selector {
                    element
                        .select(&title_sel)
                        .next()
                        .and_then(|e| e.value().attr("title"))
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| element.text().collect::<String>())
                } else {
                    element.text().collect::<String>()
                };

                let thumbnail = if let Some(thumb_sel) = &config.thumbnail_selector {
                    if let Ok(selector) = Selector::parse(thumb_sel) {
                        element
                            .select(&selector)
                            .next()
                            .and_then(|img| img.value().attr("src"))
                            .map(|s| s.to_string())
                    } else {
                        None
                    }
                } else {
                    None
                };

                let duration = if let Some(dur_sel) = &config.duration_selector {
                    if let Ok(selector) = Selector::parse(dur_sel) {
                        element
                            .select(&selector)
                            .next()
                            .map(|elem| elem.text().collect::<String>().trim().to_string())
                    } else {
                        None
                    }
                } else {
                    None
                };

                if !url.is_empty() && !title.is_empty() {
                    results.push(VideoResult {
                        title: self.clean_title(&title),
                        url,
                        thumbnail,
                        duration,
                        site: video.site.clone(),
                    });
                }
            }
        }

        Ok(results)
    }

    async fn fetch_with_retry(&self, url: &str, max_retries: u32) -> Result<String, String> {
        for attempt in 1..=max_retries {
            match self.client.get(url).send().await {
                Ok(response) => {
                    if response.status().is_success() {
                        match response.text().await {
                            Ok(text) => return Ok(text),
                            Err(e) => {
                                if attempt == max_retries {
                                    return Err(format!("Erro ao ler resposta: {}", e));
                                }
                            }
                        }
                    } else if attempt == max_retries {
                        return Err(format!("HTTP {}", response.status()));
                    }
                }
                Err(e) => {
                    if attempt == max_retries {
                        return Err(format!("Erro de conexão: {}", e));
                    }
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        }
        Err("Máximo de tentativas excedido".to_string())
    }

    fn clean_title(&self, title: &str) -> String {
        title
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace() || "()[]{}.,!?-_".contains(*c))
            .collect::<String>()
            .trim()
            .chars()
            .take(80)
            .collect()
    }

    async fn download_thumbnail(&self, url: &str) -> Result<Vec<u8>, String> {
        match self.client.get(url).send().await {
            Ok(response) => {
                if response.status().is_success() {
                    response
                        .bytes()
                        .await
                        .map(|bytes| bytes.to_vec())
                        .map_err(|e| format!("Erro ao baixar thumbnail: {}", e))
                } else {
                    Err(format!("HTTP {}", response.status()))
                }
            }
            Err(e) => Err(format!("Erro de conexão: {}", e)),
        }
    }
}

impl Default for RambleyFlixApp {
    fn default() -> Self {
        let (video_speed_tx, video_speed_rx) = mpsc::channel();
        Self::new(video_speed_tx, video_speed_rx)
    }
}

impl RambleyFlixApp {
    fn new(video_speed_tx: mpsc::Sender<VideoMessage>, video_speed_rx: mpsc::Receiver<VideoMessage>) -> Self {
        let config_dir = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".rambley_flix");
        fs::create_dir_all(&config_dir).ok();

        let (tx, rx) = mpsc::channel();

        let mut app = Self {
            scraper: Arc::new(Mutex::new(WebScraper::new())),
            users: HashMap::new(),
            current_user: None,
            login_attempts: 0,
            max_attempts: 3,
            config_dir,
            state: AppState::Login,
            username_input: String::new(),
            password_input: String::new(),
            search_input: String::new(),
            selected_site: "youtube".to_string(),
            video_results: Vec::new(),
            video_thumbnails: HashMap::new(),
            loading_search: false,
            error_message: String::new(),

            pagina_atual: 0,
            videos_por_pagina: 30,

            rt: Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("Erro ao criar runtime"),
            ),
            async_sender: tx,
            async_receiver: rx,

            video_texture: None,
            video_message_receiver: None,

            current_playing_video: None,
            video_recommendations: Vec::new(),
            loading_recommendations: false,
            recommendations_error: String::new(),

            // Novos campos
            video_speed: 1.0,
            is_playing: false,
            video_progress: 0.0,
            volume: 1.0,
            fullscreen_mode: false,
            theater_mode: false,

            user_playlists: Vec::new(),
            current_playlist: None,
            show_create_playlist_dialog: false,
            new_playlist_name: String::new(),

            app_settings: AppSettings::default(),
            duration_filter: "all".to_string(),
            sort_by: "relevance".to_string(),

            downloads_progress: HashMap::new(),
            video_notes: HashMap::new(),

            video_speed_tx,
            video_speed_rx: Some(video_speed_rx),
        };

        app.add_default_users();
        app.load_custom_users();
        app.load_user_data();
        app
    }

    fn load_user_data(&mut self) {
        let data = UserData::load(&self.config_dir);
        self.user_playlists = data.playlists;
    }

    fn add_default_users(&mut self) {
        let senha_secreta = "";

        self.users.insert(
            "".to_string(),
            User {
                username: "Decaptado".to_string(),
                password: senha_secreta.to_string(),
                access: AccessLevel::Full,
            },
        );
        self.users.insert(
            "Guest".to_string(),
            User {
                username: "Guest".to_string(),
                password: "guestpass".to_string(),
                access: AccessLevel::ViewOnly,
            },
        );
        self.users.insert(
            "Espiao".to_string(),
            User {
                username: "Espiao".to_string(),
                password: "espia123".to_string(),
                access: AccessLevel::Fake,
            },
        );
    }

    fn load_custom_users(&mut self) {
        let users_file = self.config_dir.join("usuarios.txt");
        if let Ok(content) = fs::read_to_string(&users_file) {
            for line in content.lines() {
                let parts: Vec<&str> = line.split(':').collect();
                if parts.len() == 3 {
                    let access = match parts[2].trim() {
                        "full" => AccessLevel::Full,
                        "view_only" => AccessLevel::ViewOnly,
                        "fake" => AccessLevel::Fake,
                        "netflix_only" => AccessLevel::NetflixOnly,
                        _ => AccessLevel::ViewOnly,
                    };

                    self.users.insert(
                        parts[0].to_string(),
                        User {
                            username: parts[0].to_string(),
                            password: parts[1].to_string(),
                            access,
                        },
                    );
                }
            }
        }
    }

    fn login(&mut self, username: String, password: String) -> bool {
        if let Some(user) = self.users.get(&username) {
            if user.password == password {
                self.current_user = Some(user.clone());
                self.state = AppState::MainMenu;
                self.error_message.clear();
                true
            } else {
                self.handle_failed_login();
                false
            }
        } else {
            self.handle_failed_login();
            false
        }
    }

    fn handle_failed_login(&mut self) {
        self.login_attempts += 1;
        self.error_message = format!(
            "Login inválido. Tentativas restantes: {}",
            self.max_attempts.saturating_sub(self.login_attempts)
        );

        if self.login_attempts >= self.max_attempts {
            self.error_message = "Tentativas excedidas. Saindo...".to_string();
        }
    }

    fn save_to_history(&self, url: &str) {
        let hist_file = self.config_dir.join("links.txt");
        let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S");
        let entry = format!("{} | 1x | {}\n", timestamp, url);

        if let Err(e) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&hist_file)
            .and_then(|mut f| f.write_all(entry.as_bytes()))
        {
            eprintln!("Erro ao salvar histórico: {}", e);
        }
    }

    fn start_search(&self) {
        let scraper_arc = Arc::clone(&self.scraper);
        let site = self.selected_site.clone();
        let query = self.search_input.clone();
        let sender = self.async_sender.clone();
        let videos_per_page = self.videos_por_pagina;

        self.rt.spawn(async move {
            let scraper = scraper_arc.lock().await;
            let result = scraper
                .search_videos(&site, &query, videos_per_page * 4)
                .await;

            println!("[DEBUG] Resultado da busca: {:?}", &result);
            sender.send(AsyncMessage::SearchComplete(result)).unwrap();
        });
    }

    fn start_recommendations_search(&mut self, video: VideoResult) {
        self.loading_recommendations = true;
        self.video_recommendations.clear();
        self.recommendations_error.clear();
        self.current_playing_video = Some(video.clone());

        let scraper_arc = Arc::clone(&self.scraper);
        let sender = self.async_sender.clone();

        self.rt.spawn(async move {
            let scraper = scraper_arc.lock().await;
            let result = scraper.search_recommendations(&video).await;

            sender
                .send(AsyncMessage::RecommendationsComplete(result))
                .unwrap();
        });
    }

    fn load_thumbnail(&self, video_url: &str, thumbnail_url: &str) {
        let video_url = video_url.to_string();
        let thumbnail_url = thumbnail_url.to_string();
        let scraper = Arc::clone(&self.scraper);
        let sender = self.async_sender.clone();

        self.rt.spawn(async move {
            let scraper = scraper.lock().await;
            let result = scraper.download_thumbnail(&thumbnail_url).await;
            sender
                .send(AsyncMessage::ThumbnailLoaded(video_url, result))
                .unwrap();
        });
    }

    fn create_thumbnail_texture(ctx: &egui::Context, image_data: &[u8]) -> Option<TextureHandle> {
        let image = match image::load_from_memory(image_data) {
            Ok(img) => img,
            Err(e) => {
                eprintln!("Falha ao carregar imagem: {}", e);
                return None;
            }
        };
        let image = image.to_rgba8();
        let (width, height) = image.dimensions();
        let color_image = ColorImage::from_rgba_unmultiplied([width as _, height as _], &image);
        Some(ctx.load_texture("thumbnail", color_image, egui::TextureOptions::default()))
    }

    fn handle_async_messages(&mut self, ctx: &egui::Context) {
        while let Ok(message) = self.async_receiver.try_recv() {
            match message {
                AsyncMessage::SearchComplete(result) => {
                    match result {
                        Ok(results) => {
                            self.video_results = results;
                            self.error_message.clear();
                            self.pagina_atual = 0;
                        }
                        Err(e) => {
                            self.video_results.clear();
                            self.error_message = format!("Erro na busca: {}", e);
                        }
                    }
                    self.loading_search = false;
                }
                AsyncMessage::ThumbnailLoaded(video_url, result) => {
                    if let Some(thumb_info) = self.video_thumbnails.get_mut(&video_url) {
                        if let Ok(image_data) = result {
                            if let Some(texture) = Self::create_thumbnail_texture(ctx, &image_data)
                            {
                                thumb_info.texture = Some(texture);
                            }
                        }
                        thumb_info.loading = false;
                    }
                }
                AsyncMessage::RecommendationsComplete(result) => {
                    match result {
                        Ok(results) => {
                            self.video_recommendations = results;
                            self.recommendations_error.clear();
                            for rec in &self.video_recommendations {
                                if let Some(url) = &rec.thumbnail {
                                    self.load_thumbnail(&rec.url, url);
                                }
                            }
                        }
                        Err(e) => {
                            self.video_recommendations.clear();
                            self.recommendations_error = format!("Erro nas recomendações: {}", e);
                        }
                    }
                    self.loading_recommendations = false;
                }
            }
        }
    }

    fn show_advanced_search(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("🔍 Busca Avançada", |ui| {
            ui.horizontal(|ui| {
                ui.label("Duração:");
                egui::ComboBox::from_id_salt("duration_filter")
                    .selected_text(&self.duration_filter)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.duration_filter, "all".to_string(), "Todas");
                        ui.selectable_value(&mut self.duration_filter, "short".to_string(), "Curta (< 4 min)");
                        ui.selectable_value(&mut self.duration_filter, "medium".to_string(), "Média (4-20 min)");
                        ui.selectable_value(&mut self.duration_filter, "long".to_string(), "Longa (> 20 min)");
                    });
            });

            ui.horizontal(|ui| {
                ui.label("Ordenar por:");
                egui::ComboBox::from_id_salt("sort_filter")
                    .selected_text(&self.sort_by)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.sort_by, "relevance".to_string(), "Relevância");
                        ui.selectable_value(&mut self.sort_by, "date".to_string(), "Data");
                        ui.selectable_value(&mut self.sort_by, "view_count".to_string(), "Visualizações");
                        ui.selectable_value(&mut self.sort_by, "rating".to_string(), "Avaliação");
                    });
            });
        });
    }

    fn show_favorites_in_sidebar(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        ui.collapsing("⭐ Favoritos", |ui| {
            let data = UserData::load(&self.config_dir);
            if data.favorites.is_empty() {
                ui.label("Nenhum favorito");
            } else {
                for fav in data.favorites.iter().take(5) {
                    ui.horizontal(|ui| {
                        if ui.small_button("▶").clicked() {
                            let video = VideoResult {
                                title: fav.title.clone(),
                                url: fav.url.clone(),
                                thumbnail: fav.thumbnail.clone(),
                                duration: None,
                                site: "favorito".to_string(),
                            };
                            self.play_video(video);
                        }
                        ui.label(&fav.title);
                    });
                }
                if data.favorites.len() > 5 {
                    ui.label(format!("... e mais {} favoritos", data.favorites.len() - 5));
                }
            }
        });
    }

    fn show_playlists_section(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("📝 Playlists", |ui| {
            if ui.button("+ Nova Playlist").clicked() {
                self.show_create_playlist_dialog = true;
            }

            for playlist in &self.user_playlists.clone() {
                ui.horizontal(|ui| {
                    if ui.small_button("▶").clicked() {
                        self.current_playlist = Some(playlist.clone());
                        self.state = AppState::PlaylistView;
                    }
                    ui.label(&playlist.name);
                    ui.label(format!("({} vídeos)", playlist.videos.len()));
                });
            }
        });
    }

    fn show_search_screen(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        ui.vertical_centered(|ui| {
            ui.add_space(20.0);
            ui.heading(format!("🔍 Buscar em {}", self.selected_site));
            ui.add_space(20.0);

            ui.horizontal(|ui| {
                ui.label("Buscar:");
                ui.text_edit_singleline(&mut self.search_input);

                let search_button = ui.button("🔍 Buscar");
                if search_button.clicked() && !self.search_input.is_empty() {
                    self.loading_search = true;
                    self.video_results.clear();
                    self.video_thumbnails.clear();
                    self.error_message.clear();
                    self.start_search();
                    self.state = AppState::VideoResults;
                }
            });

            ui.add_space(10.0);
            self.show_advanced_search(ui);
            ui.add_space(20.0);

            ui.horizontal(|ui| {
                ui.label("Site:");
                egui::ComboBox::from_label("")
                    .selected_text(&self.selected_site)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.selected_site,
                            "youtube".to_string(),
                            "YouTube",
                        );
                        if let Some(user) = &self.current_user {
                            if matches!(user.access, AccessLevel::Full | AccessLevel::ViewOnly) {
                                ui.selectable_value(
                                    &mut self.selected_site,
                                    "pornhub".to_string(),
                                    "PornHub",
                                );
                            }
                        }
                    });
            });

            ui.add_space(30.0);
            if ui.button("🔙 Voltar ao Menu").clicked() {
                self.state = AppState::MainMenu;
            }

            if !self.error_message.is_empty() {
                ui.add_space(20.0);
                ui.colored_label(egui::Color32::RED, &self.error_message);
            }
        });
    }

    fn show_results_screen(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.heading(format!("📺 Resultados - {}", self.selected_site));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("🔙 Voltar").clicked() {
                        self.state = AppState::VideoSearch;
                    }
                    if ui.button("🔍 Nova Busca").clicked() {
                        self.search_input.clear();
                        self.state = AppState::VideoSearch;
                    }
                });
            });

            ui.separator();

            if self.loading_search {
                ui.vertical_centered(|ui| {
                    ui.add_space(50.0);
                    ui.spinner();
                    ui.label("Carregando resultados...");
                });
                return;
            }

            if !self.error_message.is_empty() {
                ui.colored_label(egui::Color32::RED, &self.error_message);
                return;
            }

            if self.video_results.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(50.0);
                    ui.label("Nenhum resultado encontrado.");
                });
                return;
            }

            let total_videos = self.video_results.len();
            let total_paginas = (total_videos + self.videos_por_pagina - 1) / self.videos_por_pagina;

            if total_paginas > 1 {
                ui.horizontal(|ui| {
                    ui.label(format!("Página {} de {}", self.pagina_atual + 1, total_paginas));

                    if self.pagina_atual > 0 && ui.button("◀ Anterior").clicked() {
                        self.pagina_atual -= 1;
                    }

                    if self.pagina_atual + 1 < total_paginas && ui.button("Próxima ▶").clicked() {
                        self.pagina_atual += 1;
                    }
                });
                ui.separator();
            }

            let inicio = self.pagina_atual * self.videos_por_pagina;
            let fim = std::cmp::min(inicio + self.videos_por_pagina, total_videos);

            let videos_da_pagina: Vec<VideoResult> = self.video_results[inicio..fim].to_vec();

            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;

                for video in &videos_da_pagina {
                    self.show_video_item(ui, ctx, video);
                }
            });

            if total_paginas > 1 {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(format!("Mostrando {} - {} de {} vídeos", inicio + 1, fim, total_videos));

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.pagina_atual + 1 < total_paginas && ui.button("Próxima ▶").clicked() {
                            self.pagina_atual += 1;
                        }

                        if self.pagina_atual > 0 && ui.button("◀ Anterior").clicked() {
                            self.pagina_atual -= 1;
                        }
                    });
                });
            }
        });
    }

    fn show_video_item(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, video: &VideoResult) {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                let thumbnail_size = Vec2::new(120.0, 90.0);

                if let Some(thumbnail_url) = &video.thumbnail {
                    if !self.video_thumbnails.contains_key(&video.url) {
                        self.video_thumbnails.insert(
                            video.url.clone(),
                            VideoThumbnail {
                                texture: None,
                                loading: true,
                            },
                        );
                        self.load_thumbnail(&video.url, thumbnail_url);
                    }

                    if let Some(thumb_info) = self.video_thumbnails.get(&video.url) {
                        if let Some(texture) = &thumb_info.texture {
                            ui.add(
                                Image::new(texture)
                                    .fit_to_exact_size(thumbnail_size)
                                    .corner_radius(egui::CornerRadius::same(8)),
                            );
                        } else if thumb_info.loading {
                            ui.allocate_ui(thumbnail_size, |ui| {
                                ui.centered_and_justified(|ui| {
                                    ui.spinner();
                                });
                            });
                        } else {
                            ui.allocate_ui(thumbnail_size, |ui| {
                                ui.centered_and_justified(|ui| {
                                    ui.label("📷");
                                });
                            });
                        }
                    }
                } else {
                    ui.allocate_ui(thumbnail_size, |ui| {
                        ui.centered_and_justified(|ui| {
                            ui.label("📺");
                        });
                    });
                }

                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.strong(&video.title);
                        if let Some(duration) = &video.duration {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(format!("⏱ {}", duration));
                            });
                        }
                    });

                    ui.label(format!("🌐 {}", video.site));

                    ui.horizontal(|ui| {
                        let play_button = ui.button("▶ Reproduzir");

                        if play_button.clicked() {
                            if let Some(user) = &self.current_user {
                                match user.access {
                                    AccessLevel::Full | AccessLevel::ViewOnly => {
                                        self.play_video(video.clone());
                                    }
                                    AccessLevel::Fake => {
                                        self.error_message = "Acesso negado. Usuário sem permissão real.".to_string();
                                    }
                                    AccessLevel::NetflixOnly => {
                                        self.error_message = "Acesso permitido apenas ao Netflix.".to_string();
                                    }
                                    AccessLevel::None => {
                                        self.error_message = "Sem acesso.".to_string();
                                    }
                                }
                            }
                        }

                        if ui.button("⭐ Favoritar").clicked() {
                            self.add_to_favorites(video);
                        }

                        if ui.button("📋 Copiar URL").clicked() {
                            ctx.copy_text(video.url.clone());
                        }

                        if ui.button("💾 Download").clicked() {
                            self.download_video(video);
                        }
                    });
                });
            });
        });
    }

    fn show_compact_recommendation_item(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context, video: &VideoResult) {
        ui.group(|ui| {
            ui.set_width(230.0);

            let thumbnail_size = Vec2::new(80.0, 60.0);

            ui.horizontal(|ui| {
                if let Some(thumbnail_url) = &video.thumbnail {
                    if !self.video_thumbnails.contains_key(&video.url) {
                        self.video_thumbnails.insert(
                            video.url.clone(),
                            VideoThumbnail {
                                texture: None,
                                loading: true,
                            },
                        );
                        self.load_thumbnail(&video.url, thumbnail_url);
                    }

                    if let Some(thumb_info) = self.video_thumbnails.get(&video.url) {
                        if let Some(texture) = &thumb_info.texture {
                            let image = egui::Image::new(texture)
                                .fit_to_exact_size(thumbnail_size)
                                .corner_radius(egui::CornerRadius::same(4));
                            let image_button = ui.add(egui::ImageButton::new(image));
                            if image_button.clicked() {
                                self.play_video(video.clone());
                            }
                        } else if thumb_info.loading {
                            ui.allocate_ui(thumbnail_size, |ui| {
                                ui.centered_and_justified(|ui| {
                                    ui.spinner();
                                });
                            });
                        } else {
                            ui.allocate_ui(thumbnail_size, |ui| {
                                ui.centered_and_justified(|ui| {
                                    ui.label("📷");
                                });
                            });
                        }
                    }
                } else {
                    ui.allocate_ui(thumbnail_size, |ui| {
                        ui.centered_and_justified(|ui| {
                            ui.label("📺");
                        });
                    });
                }

                ui.vertical(|ui| {
                    ui.set_width(140.0);

                    let title = if video.title.len() > 50 {
                        format!("{}...", &video.title[..50])
                    } else {
                        video.title.clone()
                    };

                    let title_button = ui.button(&title);
                    if title_button.clicked() {
                        self.play_video(video.clone());
                    }

                    if let Some(duration) = &video.duration {
                        ui.label(format!("⏱ {}", duration));
                    }
                });
            });
        });
    }

    fn get_fallback_recommendations(&self) -> Vec<VideoResult> {
        // Recomendações de exemplo quando não há dados reais
        vec![
            VideoResult {
                title: "🎆 Fireworks Display 4K".to_string(),
                url: "https://example.com/fireworks".to_string(),
                thumbnail: Some("https://via.placeholder.com/160x120/FF6B6B/FFFFFF?text=Fireworks".to_string()),
                duration: Some("3:45".to_string()),
                site: "example".to_string(),
            },
            VideoResult {
                title: "🌊 Ocean Waves Relaxing".to_string(),
                url: "https://example.com/ocean".to_string(),
                thumbnail: Some("https://via.placeholder.com/160x120/4ECDC4/FFFFFF?text=Ocean".to_string()),
                duration: Some("10:20".to_string()),
                site: "example".to_string(),
            },
            VideoResult {
                title: "🎵 Lo-Fi Hip Hop Beats".to_string(),
                url: "https://example.com/lofi".to_string(),
                thumbnail: Some("https://via.placeholder.com/160x120/9B59B6/FFFFFF?text=Lo-Fi".to_string()),
                duration: Some("2:15:30".to_string()),
                site: "example".to_string(),
            },
            VideoResult {
                title: "🌮 Nature Documentary".to_string(),
                url: "https://example.com/nature".to_string(),
                thumbnail: Some("https://via.placeholder.com/160x120/27AE60/FFFFFF?text=Nature".to_string()),
                duration: Some("45:12".to_string()),
                site: "example".to_string(),
            },
            VideoResult {
                title: "🎃 Cooking Tutorial".to_string(),
                url: "https://example.com/cooking".to_string(),
                thumbnail: Some("https://via.placeholder.com/160x120/F39C12/FFFFFF?text=Cooking".to_string()),
                duration: Some("15:45".to_string()),
                site: "example".to_string(),
            },
            VideoResult {
                title: "🏆 Sports Highlights".to_string(),
                url: "https://example.com/sports".to_string(),
                thumbnail: Some("https://via.placeholder.com/160x120/E74C3C/FFFFFF?text=Sports".to_string()),
                duration: Some("8:30".to_string()),
                site: "example".to_string(),
            },
            VideoResult {
                title: "🎸 Guitar Lesson".to_string(),
                url: "https://example.com/guitar".to_string(),
                thumbnail: Some("https://via.placeholder.com/160x120/8E44AD/FFFFFF?text=Guitar".to_string()),
                duration: Some("12:05".to_string()),
                site: "example".to_string(),
            },
            VideoResult {
                title: "🎆 Space Exploration".to_string(),
                url: "https://example.com/space".to_string(),
                thumbnail: Some("https://via.placeholder.com/160x120/2C3E50/FFFFFF?text=Space".to_string()),
                duration: Some("28:40".to_string()),
                site: "example".to_string(),
            },
            VideoResult {
                title: "🎨 Art Tutorial".to_string(),
                url: "https://example.com/art".to_string(),
                thumbnail: Some("https://via.placeholder.com/160x120/E67E22/FFFFFF?text=Art".to_string()),
                duration: Some("18:22".to_string()),
                site: "example".to_string(),
            },
            VideoResult {
                title: "📚 Programming Tips".to_string(),
                url: "https://example.com/programming".to_string(),
                thumbnail: Some("https://via.placeholder.com/160x120/34495E/FFFFFF?text=Code".to_string()),
                duration: Some("25:15".to_string()),
                site: "example".to_string(),
            },
            VideoResult {
                title: "🌱 Gardening Guide".to_string(),
                url: "https://example.com/garden".to_string(),
                thumbnail: Some("https://via.placeholder.com/160x120/2ECC71/FFFFFF?text=Garden".to_string()),
                duration: Some("14:33".to_string()),
                site: "example".to_string(),
            },
            VideoResult {
                title: "🏃 Fitness Workout".to_string(),
                url: "https://example.com/fitness".to_string(),
                thumbnail: Some("https://via.placeholder.com/160x120/16A085/FFFFFF?text=Fitness".to_string()),
                duration: Some("35:20".to_string()),
                site: "example".to_string(),
            },
        ]
    }

    fn show_grid_recommendation_item(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, video: &VideoResult, item_width: f32) {
        ui.group(|ui| {
            ui.set_width(item_width);
            ui.set_height(120.0);

            ui.vertical(|ui| {
                // Thumbnail maior para layout em grid
                let thumbnail_size = Vec2::new(item_width - 20.0, 70.0);

                if let Some(thumbnail_url) = &video.thumbnail {
                    if !self.video_thumbnails.contains_key(&video.url) {
                        self.video_thumbnails.insert(
                            video.url.clone(),
                            VideoThumbnail {
                                texture: None,
                                loading: true,
                            },
                        );
                        self.load_thumbnail(&video.url, thumbnail_url);
                    }

                    if let Some(thumb_info) = self.video_thumbnails.get(&video.url) {
                        if let Some(texture) = &thumb_info.texture {
                            let image = egui::Image::new(texture)
                                .fit_to_exact_size(thumbnail_size)
                                .corner_radius(egui::CornerRadius::same(6));
                            let image_button = ui.add(egui::ImageButton::new(image));
                            if image_button.clicked() {
                                self.play_video(video.clone());
                            }
                        } else if thumb_info.loading {
                            ui.allocate_ui(thumbnail_size, |ui| {
                                ui.centered_and_justified(|ui| {
                                    ui.spinner();
                                });
                            });
                        } else {
                            ui.allocate_ui(thumbnail_size, |ui| {
                                ui.centered_and_justified(|ui| {
                                    ui.label("📷");
                                });
                            });
                        }
                    }
                } else {
                    ui.allocate_ui(thumbnail_size, |ui| {
                        ui.centered_and_justified(|ui| {
                            ui.label("📺");
                        });
                    });
                }

                // Título compacto
                let title = if video.title.len() > 35 {
                    format!("{}...", &video.title[..35])
                } else {
                    video.title.clone()
                };

                let title_button = ui.small_button(&title);
                if title_button.clicked() {
                    self.play_video(video.clone());
                }

                // Duração
                if let Some(duration) = &video.duration {
                    ui.horizontal(|ui| {
                        ui.label("⏱");
                        ui.small(&*duration);
                    });
                }
            });
        });
    }

    fn show_playing_video_screen(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.vertical(|ui| {
            // Header
            ui.horizontal(|ui| {
                if let Some(video) = &self.current_playing_video {
                    ui.heading(&video.title);
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("🔙 Voltar").clicked() {
                        self.state = AppState::VideoResults;
                        self.video_texture = None;
                        self.video_message_receiver = None;
                    }

                    if ui.button("🔳 Fullscreen").clicked() {
                        self.fullscreen_mode = !self.fullscreen_mode;
                    }

                    if ui.button("🎭 Teatro").clicked() {
                        self.theater_mode = !self.theater_mode;
                    }

                    ui.label("Velocidade:");
                    if ui.add(egui::Slider::new(&mut self.video_speed, 0.25..=4.0).step_by(0.25).text("x")).changed() {
                        let _ = self.video_speed_tx.send(VideoMessage::SetSpeed(self.video_speed));
                    }
                    if ui.button("1x").clicked() {
                        self.video_speed = 1.0;
                        let _ = self.video_speed_tx.send(VideoMessage::SetSpeed(self.video_speed));
                    }
                });
            });

            ui.separator();

            // Video Player
            let available_size = ui.available_size();
            let video_width = available_size.x;
            let video_height = video_width * 9.0 / 16.0;
            let video_size = Vec2::new(video_width, video_height);

            if let Some((tex, (orig_w, orig_h))) = &self.video_texture {
                let aspect = *orig_w as f32 / *orig_h as f32;
                let display_height = video_width / aspect;
                let final_size = Vec2::new(video_width, display_height.max(100.0));

                ui.add(
                    egui::Image::new(tex)
                        .fit_to_exact_size(final_size)
                        .corner_radius(12.0),
                );
            } else {
                ui.allocate_ui(video_size, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.spinner();
                        ui.label("🎥 Carregando vídeo...");
                    });
                });
            }

            ui.add_space(10.0);

            // Controles
            ui.horizontal(|ui| {
                if ui.button(if self.is_playing { "⏸ Pausar" } else { "▶ Play" }).clicked() {
                    self.toggle_play_pause();
                }
                if ui.button("⏩ Avançar 10s").clicked() {
                    self.seek_video(10.0);
                }
                if ui.button("⏪ Voltar 10s").clicked() {
                    self.seek_video(-10.0);
                }

                ui.add(egui::Slider::new(&mut self.volume, 0.0..=1.0).text("🔊").show_value(true));
            });

            ui.add_space(10.0);

            // Recomendações abaixo
            ui.heading("📺 Recomendações");
            ui.separator();

            egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                if self.loading_recommendations {
                    ui.spinner();
                } else {
                    let recs = self.video_recommendations.clone();
                    for video in recs.iter().take(8) {
                        self.show_compact_recommendation_item(ui, ctx, video);
                        ui.add_space(8.0);
                    }
                }
            });
        });
    }

    fn show_netflix_screen(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(50.0);
            ui.heading("🍿 Modo Netflix");
            ui.add_space(30.0);

            ui.label("🎬 Conteúdo em breve...");
            ui.add_space(20.0);
            ui.label("Esta seção estará disponível em futuras atualizações.");

            ui.add_space(50.0);

            ui.horizontal(|ui| {
                if ui.button("🔙 Voltar ao Menu").clicked() {
                    self.state = AppState::MainMenu;
                }

                if ui.button("🌐 Abrir Netflix").clicked() {
                    if let Err(e) = open::that("https://www.netflix.com") {
                        self.error_message = format!("Erro ao abrir Netflix: {}", e);
                    }
                }
            });

            if !self.error_message.is_empty() {
                ui.add_space(20.0);
                ui.colored_label(egui::Color32::RED, &self.error_message);
            }
        });
    }

    fn show_history_screen(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.heading("⏳ Histórico de Reprodução");
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                let hist_file = self.config_dir.join("links.txt");
                if let Ok(content) = fs::read_to_string(&hist_file) {
                    if content.trim().is_empty() {
                        ui.label("Seu histórico está vazio.");
                    } else {
                        for line in content.lines().rev() {
                            let parts: Vec<&str> = line.split(" | ").collect();
                            if parts.len() >= 3 {
                                let timestamp = parts[0];
                                let url = parts[2];
                                ui.horizontal(|ui| {
                                    ui.label(format!("📅 {}", timestamp));
                                    ui.hyperlink_to(url, url.to_string());
                                });
                            }
                            ui.add_space(5.0);
                        }
                    }
                } else {
                    ui.label("Não foi possível carregar o histórico.");
                }
            });
        });
    }

    fn show_downloads_screen(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.heading("💾 Downloads");
            ui.separator();

            if ui.button("📂 Abrir pasta de downloads").clicked() {
                let download_dir = self.config_dir.join("downloads");
                if let Err(e) = open::that(&download_dir) {
                    self.error_message = format!("Erro ao abrir pasta: {}", e);
                }
            }

            ui.add_space(10.0);

            if self.downloads_progress.is_empty() {
                ui.label("Nenhum download em andamento.");
            } else {
                for (title, progress) in &self.downloads_progress.clone() {
                    ui.horizontal(|ui| {
                        ui.label(title);
                        ui.add(egui::ProgressBar::new(*progress));
                    });
                }
            }
        });
    }

    fn show_settings_screen(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.heading("⚙️ Configurações");
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("Qualidade padrão:");
                egui::ComboBox::from_id_salt("default_quality")
                    .selected_text(&self.app_settings.default_quality)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.app_settings.default_quality, "1080p".to_string(), "1080p");
                        ui.selectable_value(&mut self.app_settings.default_quality, "720p".to_string(), "720p");
                        ui.selectable_value(&mut self.app_settings.default_quality, "480p".to_string(), "480p");
                        ui.selectable_value(&mut self.app_settings.default_quality, "360p".to_string(), "360p");
                    });
            });

            ui.checkbox(&mut self.app_settings.auto_play_next, "Reproduzir próximo automaticamente");
            ui.checkbox(&mut self.app_settings.download_thumbnails, "Baixar thumbnails automaticamente");

            ui.horizontal(|ui| {
                ui.label("Tema:");
                egui::ComboBox::from_id_salt("theme")
                    .selected_text(&self.app_settings.theme)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.app_settings.theme, "Dark".to_string(), "Escuro");
                        ui.selectable_value(&mut self.app_settings.theme, "Light".to_string(), "Claro");
                    });
            });

            ui.separator();
            ui.heading("Atalhos do teclado");

            ui.horizontal(|ui| {
                ui.label("Play/Pause:");
                ui.label(self.app_settings.shortcuts.get("play_pause").unwrap_or(&"Space".to_string()));
            });

            ui.horizontal(|ui| {
                ui.label("Avançar:");
                ui.label(self.app_settings.shortcuts.get("seek_forward").unwrap_or(&"ArrowRight".to_string()));
            });

            ui.add_space(20.0);
            if ui.button("💾 Salvar configurações").clicked() {
                self.save_settings();
            }
        });
    }

    fn save_settings(&self) {
        let settings_file = self.config_dir.join("settings.json");
        if let Ok(json) = serde_json::to_string_pretty(&self.app_settings) {
            let _ = fs::write(settings_file, json);
        }
    }

    fn toggle_play_pause(&mut self) {
        self.is_playing = !self.is_playing;
        let message = if self.is_playing {
            VideoMessage::Play
        } else {
            VideoMessage::Pause
        };
        let _ = self.video_speed_tx.send(message);
    }

    fn seek_video(&mut self, seconds: f64) {
        let _ = self.video_speed_tx.send(VideoMessage::Seek(seconds));
        println!("Seeking {} seconds", seconds);
    }

    fn download_video(&mut self, video: &VideoResult) {
        let download_dir = self.config_dir.join("downloads");
        fs::create_dir_all(&download_dir).ok();

        let title = video.title.clone();
        let url = video.url.clone();

        self.downloads_progress.insert(title.clone(), 0.0);

        thread::spawn(move || {
            let output = Command::new("yt-dlp")
                .arg("-o")
                .arg(format!("{}/%(title)s.%(ext)s", download_dir.display()))
                .arg(&url)
                .spawn();

            if let Ok(_) = output {
                println!("Download iniciado: {}", title);
            }
        });
    }

    fn update_last_watched(&mut self, video: &VideoResult, position: f32) {
        let mut data = UserData::load(&self.config_dir);

        data.last_watched.retain(|v| v.url != video.url);

        let meta = VideoMeta {
            url: video.url.clone(),
            title: video.title.clone(),
            thumbnail: video.thumbnail.clone(),
            last_position: position,
        };
        data.last_watched.insert(0, meta);

        if data.last_watched.len() > 10 {
            data.last_watched.remove(10);
        }

        data.save(&self.config_dir);
    }

    fn add_to_favorites(&mut self, video: &VideoResult) {
        let mut data = UserData::load(&self.config_dir);
        if !data.favorites.iter().any(|v| v.url == video.url) {
            data.favorites.push(VideoMeta {
                url: video.url.clone(),
                title: video.title.clone(),
                thumbnail: video.thumbnail.clone(),
                last_position: 0.0,
            });
            data.save(&self.config_dir);
        }
    }

    fn remove_from_favorites(&mut self, video: &VideoResult) {
        let mut data = UserData::load(&self.config_dir);
        data.favorites.retain(|v| v.url != video.url);
        data.save(&self.config_dir);
    }

    fn play_video(&mut self, video: VideoResult) {
        self.video_texture = None;
        self.video_message_receiver = None;
        self.save_to_history(&video.url);

        let video_url = video.url.clone();
        let (video_message_sender, video_message_receiver) = mpsc::channel();
        self.video_message_receiver = Some(video_message_receiver);

        self.start_recommendations_search(video.clone());
        self.state = AppState::PlayingVideo;

        let video_speed_rx = self.video_speed_rx.take();

        thread::spawn(move || {
            if let Err(e) = gst::init() {
                eprintln!("Erro ao inicializar GStreamer: {}", e);
                return;
            }

            let direct_url = match Self::get_direct_video_url(&video_url) {
                Ok(url) => url,
                Err(e) => {
                    eprintln!("Falha ao obter URL direta: {}", e);
                    return;
                }
            };

            println!("[DEBUG] URL direta do vídeo: {}", direct_url);

            let pipeline_str = format!(
                "uridecodebin uri={} ! videoconvert ! video/x-raw,format=RGBA ! appsink name=sink",
                direct_url
            );

            let pipeline = match gst::parse::launch(&pipeline_str) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("Erro ao criar pipeline GStreamer: {}", e);
                    return;
                }
            };

            let pipeline = pipeline
                .downcast::<gst::Pipeline>()
                .expect("Elemento não é uma gst::Pipeline");

            let appsink = pipeline
                .by_name("sink")
                .expect("Falha ao obter o elemento sink do pipeline")
                .downcast::<gst_app::AppSink>()
                .expect("Falha ao fazer downcast do sink para AppSink");

            appsink.set_property("emit-signals", true);
            appsink.set_property("sync", false);

            if pipeline.set_state(gst::State::Playing).is_err() {
                eprintln!("Erro ao iniciar reprodução");
                return;
            }

            // Thread para controles (play/pause/seek)
            let pipeline_clone = pipeline.clone();
            thread::spawn(move || {
                if let Some(receiver) = video_speed_rx {
                    while let Ok(message) = receiver.recv() {
                        match message {
                            VideoMessage::SetSpeed(_) => {}
                            VideoMessage::Play => {
                                let _ = pipeline_clone.set_state(gst::State::Playing);
                            }
                            VideoMessage::Pause => {
                                let _ = pipeline_clone.set_state(gst::State::Paused);
                            }
                            VideoMessage::Seek(seconds) => {
                                if let Some(duration) = pipeline_clone.query_duration::<gst::ClockTime>() {
                                    let current_pos = pipeline_clone.query_position::<gst::ClockTime>()
                                        .unwrap_or(gst::ClockTime::ZERO);

                                    let new_pos = if seconds > 0.0 {
                                        current_pos + gst::ClockTime::from_seconds(seconds as u64)
                                    } else {
                                        current_pos.saturating_sub(gst::ClockTime::from_seconds((-seconds) as u64))
                                    };

                                    let _ = pipeline_clone.seek_simple(
                                        gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                                        new_pos.min(duration)
                                    );
                                }
                            }
                            _ => {}
                        }
                    }
                }
            });

            // Captura de frames sem travar a UI
            thread::spawn(move || {
                loop {
                    match appsink.pull_sample() {
                        Ok(sample) => {
                            if let (Some(buffer), Some(caps)) = (sample.buffer(), sample.caps()) {
                                if let Some(structure) = caps.structure(0) {
                                    if let (Ok(width), Ok(height)) = (
                                        structure.get::<i32>("width"),
                                        structure.get::<i32>("height")
                                    ) {
                                        if let Ok(map) = buffer.map_readable() {
                                            let frame_data = map.as_slice().to_vec();
                                            let _ = video_message_sender.send(VideoMessage::Frame(
                                                frame_data,
                                                (width as usize, height as usize),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        Err(_) => break, // fim ou erro → sai do loop
                    }
                }
            });

            // Monitora mensagens do bus
            let bus = pipeline.bus().unwrap();
            loop {
                match bus.timed_pop(Some(gst::ClockTime::from_mseconds(50))) {
                    Some(msg) => match msg.view() {
                        gst::MessageView::Eos(..) => break,
                        gst::MessageView::Error(err) => {
                            eprintln!("Error: {}", err.error());
                            break;
                        }
                        _ => {}
                    },
                    None => {}
                }
            }

            let _ = pipeline.set_state(gst::State::Null);
        });
    }

    fn get_direct_video_url(url: &str) -> Result<String, String> {
        // Try to extract direct video URL using yt-dlp
        let output = Command::new("yt-dlp")
            .arg("-g")
            .arg("--no-playlist")
            .arg(url)
            .output();

        match output {
            Ok(result) => {
                if result.status.success() {
                    let direct_url = String::from_utf8_lossy(&result.stdout).trim().to_string();
                    if !direct_url.is_empty() {
                        Ok(direct_url)
                    } else {
                        // Fallback: return original URL
                        Ok(url.to_string())
                    }
                } else {
                    let error = String::from_utf8_lossy(&result.stderr);
                    Err(format!("yt-dlp error: {}", error))
                }
            }
            Err(e) => {
                eprintln!("yt-dlp not available: {}, using original URL", e);
                Ok(url.to_string())
            }
        }
    }

    fn create_playlist(&mut self, name: String) {
        let playlist = Playlist {
            name: name.clone(),
            videos: Vec::new(),
            created_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        };

        self.user_playlists.push(playlist);
        self.save_user_data();
    }

    fn add_to_playlist(&mut self, video: &VideoResult, playlist_name: &str) {
        if let Some(playlist) = self.user_playlists.iter_mut()
            .find(|p| p.name == playlist_name) {

            let video_meta = VideoMeta {
                url: video.url.clone(),
                title: video.title.clone(),
                thumbnail: video.thumbnail.clone(),
                last_position: 0.0,
            };

            if !playlist.videos.iter().any(|v| v.url == video.url) {
                playlist.videos.push(video_meta);
                self.save_user_data();
            }
        }
    }

    fn save_user_data(&self) {
        let user_data = UserData {
            last_watched: Vec::new(), // Load from existing data if needed
            favorites: Vec::new(),    // Load from existing data if needed
            playlists: self.user_playlists.clone(),
        };
        user_data.save(&self.config_dir);
    }

    fn show_main_menu(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        ui.vertical_centered(|ui| {
            ui.add_space(50.0);

            if let Some(user) = &self.current_user {
                ui.heading(format!("Bem-vindo, {}!", user.username));
                ui.label(format!("Nível de acesso: {:?}", user.access));
            }

            ui.add_space(30.0);

            let button_size = egui::Vec2::new(200.0, 40.0);

            if ui.add_sized(button_size, egui::Button::new("🔍 Buscar Vídeos")).clicked() {
                self.state = AppState::VideoSearch;
                self.error_message.clear();
            }

            ui.add_space(10.0);

            if let Some(user) = &self.current_user {
                if matches!(user.access, AccessLevel::NetflixOnly | AccessLevel::Full) {
                    if ui.add_sized(button_size, egui::Button::new("🍿 Netflix")).clicked() {
                        self.state = AppState::Netflix;
                    }
                    ui.add_space(10.0);
                }
            }

            if ui.add_sized(button_size, egui::Button::new("⏳ Histórico")).clicked() {
                self.state = AppState::History;
            }

            ui.add_space(10.0);

            if ui.add_sized(button_size, egui::Button::new("📝 Playlists")).clicked() {
                self.state = AppState::PlaylistView;
            }

            ui.add_space(10.0);

            if ui.add_sized(button_size, egui::Button::new("💾 Downloads")).clicked() {
                self.state = AppState::Downloads;
            }

            ui.add_space(10.0);

            if ui.add_sized(button_size, egui::Button::new("⚙️ Configurações")).clicked() {
                self.state = AppState::Settings;
            }

            ui.add_space(30.0);

            if ui.button("🚪 Sair").clicked() {
                self.current_user = None;
                self.state = AppState::Login;
                self.username_input.clear();
                self.password_input.clear();
                self.error_message.clear();
            }

            if !self.error_message.is_empty() {
                ui.add_space(20.0);
                ui.colored_label(egui::Color32::RED, &self.error_message);
            }
        });
    }

    fn show_login_screen(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(100.0);
            ui.heading("🎬 RambleyFlix");
            ui.label("Sistema de Streaming Avançado");
            ui.add_space(50.0);

            ui.horizontal(|ui| {
                ui.label("Usuário:");
                ui.text_edit_singleline(&mut self.username_input);
            });

            ui.add_space(10.0);

            ui.horizontal(|ui| {
                ui.label("Senha:  ");
                ui.add(egui::TextEdit::singleline(&mut self.password_input).password(true));
            });

            ui.add_space(20.0);

            let login_button = ui.button("🔐 Entrar");

            if login_button.clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if self.login_attempts < self.max_attempts {
                    self.login(
                        self.username_input.clone(),
                        self.password_input.clone(),
                    );
                }
            }

            if self.login_attempts >= self.max_attempts {
                ui.add_space(20.0);
                ui.colored_label(egui::Color32::RED, "Sistema bloqueado por excesso de tentativas.");

                if ui.button("🔄 Resetar").clicked() {
                    self.login_attempts = 0;
                    self.error_message.clear();
                    self.username_input.clear();
                    self.password_input.clear();
                }
            }

            if !self.error_message.is_empty() {
                ui.add_space(20.0);
                ui.colored_label(egui::Color32::RED, &self.error_message);
            }

            ui.add_space(50.0);
            ui.label("Usuários disponíveis: Decaptado, Guest, Espiao");
        });
    }
}

impl eframe::App for RambleyFlixApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_async_messages(ctx);

        // Handle video frame updates (FPS otimizado)
        if let Some(reciver) = &self.video_message_receiver {
            while let Ok(VideoMessage::Frame(frame_data, (width, height))) = reciver.try_recv() {
                let color_image = egui::ColorImage::from_rgba_unmultiplied([width, height], &frame_data);

                if let Some((texture, _)) = &mut self.video_texture {
                    texture.set(color_image, egui::TextureOptions::default());
                } else {
                    let texture = ctx.load_texture("video_frame", color_image, egui::TextureOptions::default());
                    self.video_texture = Some((texture, (width, height)));
                }
            }
        }


        // Diálogo de nova playlist
        if self.show_create_playlist_dialog {
            egui::Window::new("Nova Playlist")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Nome:");
                        ui.text_edit_singleline(&mut self.new_playlist_name);
                    });

                    ui.horizontal(|ui| {
                        if ui.button("Criar").clicked() {
                            if !self.new_playlist_name.is_empty() {
                                self.create_playlist(self.new_playlist_name.clone());
                                self.new_playlist_name.clear();
                                self.show_create_playlist_dialog = false;
                            }
                        }

                        if ui.button("Cancelar").clicked() {
                            self.new_playlist_name.clear();
                            self.show_create_playlist_dialog = false;
                        }
                    });
                });
        }

        // Sidebar
        egui::SidePanel::left("sidebar").show(ctx, |ui| {
            ui.heading("📚 Biblioteca");
            ui.separator();
            self.show_favorites_in_sidebar(ui, ctx);
            self.show_playlists_section(ui);
        });

        // Painel central
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.state {
                AppState::Login => self.show_login_screen(ui),
                AppState::MainMenu => self.show_main_menu(ui, ctx),
                AppState::VideoSearch => self.show_search_screen(ui, ctx),
                AppState::VideoResults => self.show_results_screen(ui, ctx),
                AppState::PlayingVideo => self.show_playing_video_screen(ui, ctx),
                AppState::Netflix => self.show_netflix_screen(ui),
                AppState::History => self.show_history_screen(ui),
                AppState::Downloads => self.show_downloads_screen(ui),
                AppState::Settings => self.show_settings_screen(ui),
                AppState::PlaylistView => {
                    ui.heading("📝 Playlists");
                    ui.separator();
                }
            }
        });
    }
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([800.0, 600.0]),
        ..Default::default()
    };

    let (tx, rx) = std::sync::mpsc::channel();

    eframe::run_native(
        "RBFlix - Sistema de Streaming",
        options,
        Box::new(|_cc| {
            Ok(Box::new(RambleyFlixApp::new(tx, rx)))
        }),
    )
}
