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
use std::sync::{Arc, Mutex};
use std::thread;
use tokio::runtime::Runtime;
use tokio::sync::Mutex as TokioMutex;

// Mensagens assíncronas
enum AsyncMessage {
    SearchComplete(Result<Vec<VideoResult>, String>),
    ThumbnailLoaded(String, Result<Vec<u8>, String>),
    RecommendationsComplete(Result<Vec<VideoResult>, String>),
}

// Level de acesso
#[derive(Debug, Clone, PartialEq)]
enum AccessLevel {
    Full,
    ViewOnly,
    Fake,
    NetflixOnly,
    None,
}

// Resultado de vídeo
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VideoResult {
    title: String,
    url: String,
    thumbnail: Option<String>,
    duration: Option<String>,
    site: String,
}

// Metadata de vídeo
#[derive(Serialize, Deserialize, Clone)]
struct VideoMeta {
    url: String,
    title: String,
    thumbnail: Option<String>,
    last_position: f32,
}

// Playlist
#[derive(Serialize, Deserialize, Clone)]
struct Playlist {
    name: String,
    videos: Vec<VideoMeta>,
    created_at: String,
}

// Dados do usuário
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
            serde_json::from_str(&content).unwrap_or_else(|_| Self::default())
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

// Configurações do app
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

// Config de sites
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

// Scraper
struct WebScraper {
    client: reqwest::Client,
    site_configs: HashMap<String, SiteConfig>,
}

// Usuário
#[derive(Debug, Clone)]
struct User {
    username: String,
    password: String,
    access: AccessLevel,
}

// Thumbnail do vídeo
#[allow(dead_code)]
struct VideoThumbnail {
    texture: Option<TextureHandle>,
    loading: bool,
}

// Estado do app
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
    ActivePlaylistView,
}

// Mensagens de controle do vídeo
#[derive(Debug)]
enum VideoCommand {
    Play,
    Pause,
    Seek(i64), // Em nanossegundos
    SetSpeed(f64),
    Stop,
}

// O app principal
struct RambleyFlixApp {
    scraper: Arc<TokioMutex<WebScraper>>,
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

    current_playing_video: Option<VideoResult>,
    video_recommendations: Vec<VideoResult>,
    loading_recommendations: bool,
    recommendations_error: String,

    // --- Campos do Player de Vídeo (Refatorados) ---
    video_texture: Option<TextureHandle>,
    // Buffer compartilhado para o frame mais recente
    current_frame: Option<Arc<Mutex<Option<ColorImage>>>>,
    // Canal para enviar comandos ao thread do GStreamer
    video_command_tx: Option<mpsc::Sender<VideoCommand>>,
    video_thread_handle: Option<thread::JoinHandle<()>>,
    // ------------------------------------------------
    video_speed: f64,
    is_playing: bool,
    video_progress: f32,
    volume: f32,
    fullscreen_mode: bool,
    theater_mode: bool,

    user_playlists: Vec<Playlist>,
    current_playlist: Option<Playlist>,
    show_create_playlist_dialog: bool,
    new_playlist_name: String,

    app_settings: AppSettings,
    duration_filter: String,
    sort_by: String,

    downloads_progress: HashMap<String, f32>,
    video_notes: HashMap<String, String>,
}

// Implementação do scraper
impl WebScraper {
    fn new() -> Self {
        let client_builder = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
            .timeout(std::time::Duration::from_secs(30));

        let client = client_builder.build().unwrap();
        let mut site_configs = HashMap::new();

        // Configurações de sites
        site_configs.insert(
            "youtube".to_string(),
            SiteConfig {
                base_url: "https://www.youtube.com".to_string(),
                search_path: "/results?search_query=".to_string(),
                video_selector: "ytd-video-renderer a#thumbnail".to_string(),
                title_selector: "a#video-title".to_string(),
                thumbnail_selector: Some("yt-image img".to_string()),
                duration_selector: Some("ytd-thumbnail-overlay-time-status-renderer".to_string()),
                url_transform: Some(|href: &str| {
                    if href.starts_with('/') {
                        format!("https://www.youtube.com{}", href)
                    } else {
                        href.to_string()
                    }
                }),
                recommendations_path: None,
                recommendations_selector: Some("ytd-compact-video-renderer".to_string()),
            },
        );

        site_configs.insert(
            "pornhub".to_string(),
            SiteConfig {
                base_url: "https://rt.pornhub.com".to_string(),
                search_path: "/video/search?search=".to_string(),
                video_selector: "div.phimage a".to_string(),
                title_selector: "span.title a".to_string(),
                thumbnail_selector: Some("img".to_string()),
                duration_selector: Some("var.duration".to_string()),
                url_transform: Some(|href: &str| format!("https://rt.pornhub.com{}", href)),
                recommendations_path: None,
                recommendations_selector: Some("div.phimage a".to_string()),
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

        let html_content = self.fetch_with_retry(&search_url, 3).await?;
        let document = Html::parse_document(&html_content);

        let video_container_selector = Selector::parse(&config.video_selector)
            .map_err(|e| format!("Erro no seletor de container: {:?}", e))?;

        let mut results = Vec::new();

        for element in document.select(&video_container_selector).take(limit) {
            let href = element.value().attr("href").unwrap_or("").trim();
            if href.is_empty() {
                continue;
            }

            let url = (config.url_transform.unwrap_or(|s| s.to_string()))(href);

            let title_selector = Selector::parse(&config.title_selector)
                .map_err(|e| format!("Erro no seletor de título: {:?}", e))?;

            let title = element
                .select(&title_selector)
                .next()
                .map(|e| e.text().collect::<String>().trim().to_string())
                .or_else(|| element.value().attr("title").map(|s| s.trim().to_string()))
                .unwrap_or_default();

            let thumbnail = config.thumbnail_selector.as_ref().and_then(|sel| {
                Selector::parse(sel).ok().and_then(|selector| {
                    element
                        .select(&selector)
                        .next()
                        .and_then(|img| img.value().attr("src"))
                        .map(String::from)
                })
            });

            let duration = config.duration_selector.as_ref().and_then(|sel| {
                Selector::parse(sel).ok().and_then(|selector| {
                    element
                        .select(&selector)
                        .next()
                        .map(|d| d.text().collect::<String>().trim().to_string())
                })
            });

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

            for element in document.select(&recs_selector).take(10) {
                let href = element.value().attr("href").unwrap_or("").trim();
                if href.is_empty() {
                    continue;
                }

                let url = (config.url_transform.unwrap_or(|s| s.to_string()))(href);

                let title_selector = Selector::parse(&config.title_selector).ok();
                let title = title_selector
                    .and_then(|sel| {
                        element
                            .select(&sel)
                            .next()
                            .map(|t| t.text().collect::<String>().trim().to_string())
                    })
                    .unwrap_or_default();

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

impl RambleyFlixApp {
    fn new() -> Self {
        let config_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".rambley_flix");
        fs::create_dir_all(&config_dir).ok();

        let (tx, rx) = mpsc::channel();

        let mut app = Self {
            scraper: Arc::new(TokioMutex::new(WebScraper::new())),
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

            current_playing_video: None,
            video_recommendations: Vec::new(),
            loading_recommendations: false,
            recommendations_error: String::new(),

            video_texture: None,
            current_frame: None,
            video_command_tx: None,
            video_thread_handle: None,

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
                username: "".to_string(),
                password: "".to_string(),
                access: AccessLevel::Full,
            },
        );

        self.users.insert(
            "Decaptado".to_string(),
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
            let _ = sender.send(AsyncMessage::SearchComplete(result));
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
            let _ = sender.send(AsyncMessage::RecommendationsComplete(result));
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
            let _ = sender.send(AsyncMessage::ThumbnailLoaded(video_url, result));
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
                        ui.selectable_value(
                            &mut self.duration_filter,
                            "short".to_string(),
                            "Curta (< 4 min)",
                        );
                        ui.selectable_value(
                            &mut self.duration_filter,
                            "medium".to_string(),
                            "Média (4-20 min)",
                        );
                        ui.selectable_value(
                            &mut self.duration_filter,
                            "long".to_string(),
                            "Longa (> 20 min)",
                        );
                    });
            });

            ui.horizontal(|ui| {
                ui.label("Ordenar por:");
                egui::ComboBox::from_id_salt("sort_filter")
                    .selected_text(&self.sort_by)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.sort_by,
                            "relevance".to_string(),
                            "Relevância",
                        );
                        ui.selectable_value(&mut self.sort_by, "date".to_string(), "Data");
                        ui.selectable_value(
                            &mut self.sort_by,
                            "view_count".to_string(),
                            "Visualizações",
                        );
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
                let response = ui.text_edit_singleline(&mut self.search_input);

                let search_button = ui.button("🔍 Buscar");
                if (search_button.clicked()
                    || response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    && !self.search_input.is_empty()
                {
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
                            if matches!(user.access, AccessLevel::Full) {
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
            let total_paginas =
                (total_videos + self.videos_por_pagina - 1) / self.videos_por_pagina;

            if total_paginas > 1 {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "Página {} de {}",
                        self.pagina_atual + 1,
                        total_paginas
                    ));

                    if self.pagina_atual > 0 && ui.button("◀ Anterior").clicked() {
                        self.pagina_atual -= 1;
                    }

                    if self.pagina_atual + 1 < total_paginas && ui.button("Próxima ▶").clicked()
                    {
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
                    ui.label(format!(
                        "Mostrando {} - {} de {} vídeos",
                        inicio + 1,
                        fim,
                        total_videos
                    ));

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.pagina_atual + 1 < total_paginas && ui.button("Próxima ▶").clicked()
                        {
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
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(format!("⏱ {}", duration));
                                },
                            );
                        }
                    });
                    ui.label(format!("🌐 {}", video.site));

                    ui.horizontal(|ui| {
                        // Botão para reproduzir o vídeo
                        let play_button = ui.button("▶ Reproduzir");
                        if play_button.clicked() {
                            if let Some(user) = &self.current_user {
                                match user.access {
                                    AccessLevel::Full | AccessLevel::ViewOnly => {
                                        self.play_video(video.clone());
                                    }
                                    AccessLevel::Fake => {
                                        self.error_message =
                                            "Acesso negado. Usuário sem permissão real."
                                                .to_string();
                                    }
                                    AccessLevel::NetflixOnly => {
                                        self.error_message =
                                            "Acesso permitido apenas ao Netflix.".to_string();
                                    }
                                    AccessLevel::None => {
                                        self.error_message = "Sem acesso.".to_string();
                                    }
                                }
                            }
                        }

                        play_button.context_menu(|ui| {
                            ui.menu_button("Adicionar à Playlist", |ui| {
                                for playlist in &self.user_playlists.clone() {
                                    if ui.button(&playlist.name).clicked() {
                                        self.add_to_playlist(video, &playlist.name);
                                        ui.close_menu();
                                    }
                                }
                            });
                        });

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

    fn show_compact_recommendation_item(
        &mut self,
        ui: &mut egui::Ui,
        _ctx: &egui::Context,
        video: &VideoResult,
    ) {
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

    fn stop_video_playback(&mut self) {
        if let Some(tx) = self.video_command_tx.take() {
            // Envia o comando para parar, ignora o erro se o receptor já morreu
            let _ = tx.send(VideoCommand::Stop);
        }
        if let Some(handle) = self.video_thread_handle.take() {
            // Espera o thread do GStreamer terminar
            let _ = handle.join();
        }
        // Limpa os recursos da UI
        self.video_texture = None;
        self.current_frame = None;
        self.current_playing_video = None;
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
                        self.stop_video_playback();
                        self.state = AppState::VideoResults;
                        return; // Retorna para evitar processar o resto da UI neste frame
                    }

                    if ui.button("🔳 Fullscreen").clicked() {
                        self.fullscreen_mode = !self.fullscreen_mode;
                    }

                    if ui.button("🎭 Teatro").clicked() {
                        self.theater_mode = !self.theater_mode;
                    }

                    ui.label("Velocidade:");
                    if ui
                        .add(
                            egui::Slider::new(&mut self.video_speed, 0.25..=4.0)
                                .step_by(0.25)
                                .text("x"),
                        )
                        .changed()
                    {
                        if let Some(tx) = &self.video_command_tx {
                            let _ = tx.send(VideoCommand::SetSpeed(self.video_speed));
                        }
                    }
                    if ui.button("1x").clicked() {
                        self.video_speed = 1.0;
                        if let Some(tx) = &self.video_command_tx {
                            let _ = tx.send(VideoCommand::SetSpeed(self.video_speed));
                        }
                    }
                });
            });

            ui.separator();

            // Video Player
            let available_size = ui.available_size_before_wrap();
            let video_width = available_size.x;
            let video_height = video_width * 9.0 / 16.0;
            let video_size = Vec2::new(video_width, video_height);

            if let Some(texture) = &self.video_texture {
                let tex_size = texture.size_vec2();
                let aspect_ratio = tex_size.x / tex_size.y;
                let display_height = video_width / aspect_ratio;
                let final_size = Vec2::new(video_width, display_height.min(available_size.y));

                ui.add(Image::new(texture).fit_to_exact_size(final_size));
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
                if ui
                    .button(if self.is_playing {
                        "⏸ Pausar"
                    } else {
                        "▶ Play"
                    })
                    .clicked()
                {
                    self.toggle_play_pause();
                }
                if ui.button("⏩ Avançar 10s").clicked() {
                    self.seek_video(10);
                }
                if ui.button("⏪ Voltar 10s").clicked() {
                    self.seek_video(-10);
                }

                ui.add(
                    egui::Slider::new(&mut self.volume, 0.0..=1.0)
                        .text("🔊")
                        .show_value(true),
                );
            });

            ui.add_space(10.0);

            // Recomendações abaixo
            ui.heading("📺 Recomendações");
            ui.separator();

            egui::ScrollArea::vertical()
                .max_height(300.0)
                .show(ui, |ui| {
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
                if ui.button("🔙 Voltar ao Menu").clicked() {
                    self.state = AppState::MainMenu;
                }
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
            if ui.button("🔙 Voltar ao Menu").clicked() {
                self.state = AppState::MainMenu;
            }
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

            if ui.button("🔙 Voltar ao Menu").clicked() {
                self.state = AppState::MainMenu;
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
                        ui.selectable_value(
                            &mut self.app_settings.default_quality,
                            "1080p".to_string(),
                            "1080p",
                        );
                        ui.selectable_value(
                            &mut self.app_settings.default_quality,
                            "720p".to_string(),
                            "720p",
                        );
                        ui.selectable_value(
                            &mut self.app_settings.default_quality,
                            "480p".to_string(),
                            "480p",
                        );
                        ui.selectable_value(
                            &mut self.app_settings.default_quality,
                            "360p".to_string(),
                            "360p",
                        );
                    });

                if ui.button("🔙 Voltar ao Menu").clicked() {
                    self.state = AppState::MainMenu;
                }
            });

            ui.checkbox(
                &mut self.app_settings.auto_play_next,
                "Reproduzir próximo automaticamente",
            );
            ui.checkbox(
                &mut self.app_settings.download_thumbnails,
                "Baixar thumbnails automaticamente",
            );

            ui.horizontal(|ui| {
                ui.label("Tema:");
                egui::ComboBox::from_id_salt("theme")
                    .selected_text(&self.app_settings.theme)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.app_settings.theme,
                            "Dark".to_string(),
                            "Escuro",
                        );
                        ui.selectable_value(
                            &mut self.app_settings.theme,
                            "Light".to_string(),
                            "Claro",
                        );
                    });
            });

            ui.separator();
            ui.heading("Atalhos do teclado");

            ui.horizontal(|ui| {
                ui.label("Play/Pause:");
                ui.label(
                    self.app_settings
                        .shortcuts
                        .get("play_pause")
                        .unwrap_or(&"Space".to_string()),
                );
            });

            ui.horizontal(|ui| {
                ui.label("Avançar:");
                ui.label(
                    self.app_settings
                        .shortcuts
                        .get("seek_forward")
                        .unwrap_or(&"ArrowRight".to_string()),
                );
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
        let command = if self.is_playing {
            VideoCommand::Play
        } else {
            VideoCommand::Pause
        };
        if let Some(tx) = &self.video_command_tx {
            let _ = tx.send(command);
        }
    }

    fn seek_video(&mut self, seconds: i64) {
        if let Some(tx) = &self.video_command_tx {
            // Converte segundos para nanossegundos para o GStreamer
            let nanos = seconds * 1_000_000_000;
            let _ = tx.send(VideoCommand::Seek(nanos));
        }
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

    // --- FUNÇÃO play_video COMPLETAMENTE REFEITA ---
    fn play_video(&mut self, video: VideoResult) {
        // 1. Limpa qualquer recurso de vídeo anterior
        self.stop_video_playback();
        self.save_to_history(&video.url);

        // 2. Prepara o estado para o novo vídeo
        self.is_playing = true;
        self.start_recommendations_search(video.clone());
        self.state = AppState::PlayingVideo;

        let video_url = video.url.clone();
        let (command_tx, command_rx) = mpsc::channel::<VideoCommand>();
        self.video_command_tx = Some(command_tx);

        let frame_buffer = Arc::new(Mutex::new(None));
        self.current_frame = Some(frame_buffer.clone());

        // 3. Spawna o thread do GStreamer
        self.video_thread_handle = Some(thread::spawn(move || {
            // Inicializa GStreamer
            if let Err(e) = gst::init() {
                eprintln!("Erro ao inicializar GStreamer: {}", e);
                return;
            }

            // Obtém a URL direta do vídeo
            let direct_url = match Self::get_direct_video_url(&video_url) {
                Ok(url) => url,
                Err(e) => {
                    eprintln!("Falha ao obter URL direta: {}", e);
                    return;
                }
            };

            // Constrói o pipeline do GStreamer
            let pipeline_str = format!(
                "uridecodebin uri={} ! videoconvert ! videoscale ! video/x-raw,format=RGBA,width=1280,height=720 ! appsink name=sink",
                direct_url
            );

            let pipeline = match gst::parse::launch(&pipeline_str) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("Erro ao criar pipeline GStreamer: {}: {}", e, pipeline_str);
                    return;
                }
            };
            let pipeline = pipeline.downcast::<gst::Pipeline>().unwrap();

            // Configura o appsink
            let appsink = pipeline
                .by_name("sink")
                .unwrap()
                .downcast::<gst_app::AppSink>()
                .unwrap();

            // Corrigido: `new_simple` para `Caps::builder`
            let caps = gst::Caps::builder("video/x-raw")
                .field("format", &"RGBA")
                .field("width", &1280)
                .field("height", &720)
                .build();
            appsink.set_caps(Some(&caps));

            let frame_buffer_clone = frame_buffer.clone();

            // Corrigido: `set_callback` para `set_callbacks`
            appsink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                        let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                        let s = caps.structure(0).ok_or(gst::FlowError::Error)?;
                        let width =
                            s.get::<i32>("width").map_err(|_| gst::FlowError::Error)? as usize;
                        let height =
                            s.get::<i32>("height").map_err(|_| gst::FlowError::Error)? as usize;

                        let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;

                        // Cria a ColorImage diretamente
                        let color_image =
                            ColorImage::from_rgba_unmultiplied([width, height], map.as_slice());

                        // Coloca no buffer compartilhado
                        *frame_buffer_clone.lock().unwrap() = Some(color_image);

                        // Corrigido: Retorna `FlowSuccess::Ok`
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );

            // Inicia a reprodução
            pipeline
                .set_state(gst::State::Playing)
                .expect("Não foi possível iniciar o pipeline");

            let bus = pipeline.bus().unwrap();

            // Loop principal do thread: escuta por mensagens no bus e comandos da UI
            'main_loop: loop {
                // Processa mensagens do GStreamer (EOS, Error)
                if let Some(msg) = bus.timed_pop(Some(gst::ClockTime::from_mseconds(10))) {
                    match msg.view() {
                        gst::MessageView::Eos(..) => {
                            println!("Fim do stream (EOS)");
                            break 'main_loop;
                        }
                        gst::MessageView::Error(err) => {
                            // Corrigido: `err.debug_info()` para `err.debug()`
                            eprintln!("Erro do pipeline: {} ({:?})", err.error(), err.debug());
                            break 'main_loop;
                        }
                        _ => {}
                    }
                }

                // Processa comandos da UI (Play, Pause, Seek, Stop)
                match command_rx.try_recv() {
                    Ok(command) => {
                        match command {
                            VideoCommand::Play => {
                                pipeline.set_state(gst::State::Playing).ok();
                            }
                            VideoCommand::Pause => {
                                pipeline.set_state(gst::State::Paused).ok();
                            }
                            VideoCommand::SetSpeed(rate) => {
                                // Corrigido: Nova forma de criar o evento de seek
                                let seek_event = gst::event::Seek::new(
                                    rate,
                                    gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                                    gst::SeekType::None,
                                    gst::ClockTime::ZERO,
                                    gst::SeekType::None,
                                    gst::ClockTime::NONE,
                                );
                                pipeline.send_event(seek_event);
                            }
                            VideoCommand::Seek(nanos) => {
                                if let Some(current_pos) =
                                    pipeline.query_position::<gst::ClockTime>()
                                {
                                    let new_pos = if nanos > 0 {
                                        current_pos.saturating_add(gst::ClockTime::from_nseconds(
                                            nanos as u64,
                                        ))
                                    } else {
                                        current_pos.saturating_sub(gst::ClockTime::from_nseconds(
                                            -nanos as u64,
                                        ))
                                    };
                                    pipeline
                                        .seek_simple(
                                            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                                            new_pos,
                                        )
                                        .ok();
                                }
                            }
                            VideoCommand::Stop => {
                                break 'main_loop;
                            }
                        }
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        println!("Canal de comando desconectado, encerrando thread.");
                        break 'main_loop;
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }

            // Limpeza final
            pipeline
                .set_state(gst::State::Null)
                .expect("Não foi possível parar o pipeline");
            println!("Thread do GStreamer finalizado.");
        }));
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
                eprintln!("yt-dlp não encontrado, usando URL original: {}", e);
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
        if let Some(playlist) = self
            .user_playlists
            .iter_mut()
            .find(|p| p.name == playlist_name)
        {
            let video_meta = VideoMeta {
                url: video.url.clone(),
                title: video.title.clone(),
                thumbnail: video.thumbnail.clone(),
                last_position: 0.0,
            };

            if !playlist.videos.iter().any(|v| v.url == video.url) {
                playlist.videos.push(video_meta);
                self.save_user_data();
                println!(
                    "Vídeo '{}' adicionado à playlist '{}'.",
                    video.title, playlist_name
                );
            } else {
                println!(
                    "Vídeo '{}' já está na playlist '{}'.",
                    video.title, playlist_name
                );
            }
        }
    }

    fn save_user_data(&self) {
        let data = UserData {
            last_watched: Vec::new(), // A tua lógica para o histórico
            favorites: Vec::new(),    // A tua lógica para os favoritos
            playlists: self.user_playlists.clone(),
        };
        data.save(&self.config_dir);
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

            if ui
                .add_sized(button_size, egui::Button::new("🔍 Buscar Vídeos"))
                .clicked()
            {
                self.state = AppState::VideoSearch;
                self.error_message.clear();
            }

            ui.add_space(10.0);

            if let Some(user) = &self.current_user {
                if matches!(user.access, AccessLevel::NetflixOnly | AccessLevel::Full) {
                    if ui
                        .add_sized(button_size, egui::Button::new("🍿 Netflix"))
                        .clicked()
                    {
                        self.state = AppState::Netflix;
                    }
                    ui.add_space(10.0);
                }
            }

            if ui
                .add_sized(button_size, egui::Button::new("⏳ Histórico"))
                .clicked()
            {
                self.state = AppState::History;
            }

            ui.add_space(10.0);

            if ui
                .add_sized(button_size, egui::Button::new("📝 Playlists"))
                .clicked()
            {
                self.state = AppState::PlaylistView;
            }

            ui.add_space(10.0);

            if ui
                .add_sized(button_size, egui::Button::new("💾 Downloads"))
                .clicked()
            {
                self.state = AppState::Downloads;
            }

            ui.add_space(10.0);

            if ui
                .add_sized(button_size, egui::Button::new("⚙️ Configurações"))
                .clicked()
            {
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
                    self.login(self.username_input.clone(), self.password_input.clone());
                }
            }

            if self.login_attempts >= self.max_attempts {
                ui.add_space(20.0);
                ui.colored_label(
                    egui::Color32::RED,
                    "Sistema bloqueado por excesso de tentativas.",
                );

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

    fn show_playlist_view(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("🔙 Voltar ao Menu").clicked() {
                self.state = AppState::MainMenu;
            }
            ui.heading("📝 Minhas Playlists");
        });
        ui.separator();
        ui.add_space(10.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            if self.user_playlists.is_empty() {
                ui.label("Ainda não tens nenhuma playlist.");
            } else {
                for playlist in self.user_playlists.clone() {
                    if ui.button(&playlist.name).clicked() {
                        self.current_playlist = Some(playlist.clone());
                        self.state = AppState::ActivePlaylistView;
                    }
                }
            }
        });
    }

    fn show_active_playlist_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            if ui.button("🔙 Voltar").clicked() {
                self.state = AppState::PlaylistView;
            }
            if let Some(playlist) = &self.current_playlist {
                ui.heading(format!("▶ {}", playlist.name));
            }
        });
        ui.separator();
        ui.add_space(10.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            if let Some(playlist) = &self.current_playlist {
                if playlist.videos.is_empty() {
                    ui.label("Esta playlist está vazia.");
                } else {
                    let videos_as_results: Vec<VideoResult> = playlist
                        .videos
                        .iter()
                        .map(|meta| {
                            VideoResult {
                                title: meta.title.clone(),
                                url: meta.url.clone(),
                                thumbnail: meta.thumbnail.clone(),
                                duration: None,
                                site: "Local".to_string(), // Marcador para vídeos locais/salvos
                            }
                        })
                        .collect();

                    for video in videos_as_results.iter() {
                        self.show_video_item(ui, ctx, video);
                        ui.add_space(5.0);
                    }
                }
            }
        });
    }
}

impl Default for RambleyFlixApp {
    fn default() -> Self {
        Self::new()
    }
}

impl eframe::App for RambleyFlixApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Pede um repaint constante quando o vídeo está tocando para manter a fluidez
        if self.state == AppState::PlayingVideo {
            ctx.request_repaint();
        }

        self.handle_async_messages(ctx);

        // --- LÓGICA DE ATUALIZAÇÃO DA TEXTURA DO VÍDEO (REFEITA) ---
        if let Some(frame_buffer) = &self.current_frame {
            // Tenta travar o mutex sem bloquear a UI
            if let Ok(mut guard) = frame_buffer.try_lock() {
                // Se houver um novo frame, pega ele
                if let Some(new_frame) = guard.take() {
                    // Se a textura já existe, atualiza os dados dela
                    if let Some(texture) = &mut self.video_texture {
                        texture.set(new_frame, egui::TextureOptions::LINEAR);
                    } else {
                        // Senão, cria uma nova textura
                        let texture = ctx.load_texture(
                            "video_frame",
                            new_frame,
                            egui::TextureOptions::LINEAR,
                        );
                        self.video_texture = Some(texture);
                    }
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

        // Renderiza a UI apenas se não estivermos em modo fullscreen
        if !self.fullscreen_mode {
            // Sidebar
            if self.state != AppState::Login {
                egui::SidePanel::left("sidebar").show(ctx, |ui| {
                    ui.heading("📚 Biblioteca");
                    ui.separator();
                    self.show_favorites_in_sidebar(ui, ctx);
                    self.show_playlists_section(ui);
                    if ui.button("🔙 Voltar ao Menu").clicked() {
                        self.state = AppState::MainMenu;
                    }
                });
            }

            // Painel central
            egui::CentralPanel::default().show(ctx, |ui| match self.state {
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
                    self.show_playlist_view(ui);
                }
                AppState::ActivePlaylistView => self.show_active_playlist_view(ui, ctx),
            });
        } else {
            egui::CentralPanel::default().show(ctx, |ui| {
                if self.state == AppState::PlayingVideo {
                    self.show_playing_video_screen(ui, ctx)
                } else {
                    self.fullscreen_mode = false;
                }
            });
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop_video_playback();
    }
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([800.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "RBFlix - Sistema de Streaming",
        options,
        Box::new(|_cc| Ok(Box::new(RambleyFlixApp::new()))),
    )
}
