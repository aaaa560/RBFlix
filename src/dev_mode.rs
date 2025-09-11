// src/dev_mode.rs
use std::sync::{Arc, mpsc};
use tokio::sync::Mutex as TokioMutex;
use egui::{Ui, Context, Color32};
use scraper::{Html, Selector};
use crate::scraper::{WebScraper, SiteConfig};
use crate::video::VideoResult;
use crate::encrypt;

// Estado do modo Dev
#[derive(Default)]
pub struct DevModeState {
    pub new_site_name: String,
    pub new_site_base_url: String,
    pub new_site_search_path: String,
    pub new_site_video_selector: String,
    pub new_site_title_selector: String,
    pub new_site_thumbnail_selector: Option<String>,
    pub new_site_duration_selector: Option<String>,
    pub new_site_url_transform: Option<String>,
    pub new_site_recommendations_selector: Option<String>,
    pub test_url: String,
    pub test_results: Vec<VideoResult>,
    pub error_message: String,
}

// Enum para mensagens de atualização da UI
#[derive(Debug)]
pub enum UIMessage {
    UpdateTestResults(Vec<VideoResult>),
    UpdateErrorMessage(String),
}

pub struct DevMode {
    pub state: DevModeState,
    pub ui_tx: mpsc::Sender<UIMessage>,
    pub ui_rx: mpsc::Receiver<UIMessage>,
}

impl DevMode {
    pub fn new() -> (Self, mpsc::Receiver<UIMessage>) {
        let (ui_tx, ui_rx) = mpsc::channel();
        let (internal_tx, internal_rx) = mpsc::channel();
        (
            Self {
                state: DevModeState::default(),
                ui_tx: internal_tx,
                ui_rx: internal_rx,
            },
            ui_rx,
        )
    }

    pub fn show(&mut self, ui: &mut Ui, ctx: &Context, scraper: Arc<TokioMutex<WebScraper>>) {
        ui.heading("🛠 Modo Desenvolvedor - Adicionar Novo Site");
        ui.separator();

        // Formulário para adicionar um novo site
        ui.label("Nome do site:");
        ui.text_edit_singleline(&mut self.state.new_site_name);

        ui.label("URL base (ex: https://exemplo.com):");
        ui.text_edit_singleline(&mut self.state.new_site_base_url);

        ui.label("Caminho de busca (ex: /search?q=):");
        ui.text_edit_singleline(&mut self.state.new_site_search_path);

        ui.label("Seletor de vídeos (ex: div.video):");
        ui.text_edit_singleline(&mut self.state.new_site_video_selector);

        ui.label("Seletor de título (ex: h3.title):");
        ui.text_edit_singleline(&mut self.state.new_site_title_selector);

        ui.label("Seletor de thumbnail (opcional, ex: img.thumbnail):");
        ui.text_edit_singleline(
            self.state.new_site_thumbnail_selector
                .get_or_insert_with(|| String::new())
        );

        ui.label("Seletor de duração (opcional, ex: span.duration):");
        ui.text_edit_singleline(
            self.state.new_site_duration_selector
                .get_or_insert_with(|| String::new())
        );

        ui.label("Transformação de URL (opcional, ex: format!(\"https://exemplo.com{}\", href)):");
        ui.text_edit_singleline(
            self.state.new_site_url_transform
                .get_or_insert_with(|| String::new())
        );

        ui.label("Seletor de recomendações (opcional, ex: div.recommended):");
        ui.text_edit_singleline(
            self.state.new_site_recommendations_selector
                .get_or_insert_with(|| String::new())
        );

        // Botão para adicionar o site
        if ui.button("🔧 Adicionar Site").clicked() {
            if let Err(e) = self.add_new_site(scraper.clone()) {
                self.state.error_message = format!("Erro ao adicionar site: {}", e);
            }
        }

        ui.separator();

        // Seção para testar seletores
        ui.heading("🧪 Testar Seletores");
        ui.label("URL de teste (ex: https://exemplo.com/search?q=test):");
        ui.text_edit_singleline(&mut self.state.test_url);

        if ui.button("🔍 Testar").clicked() {
            self.test_selectors(ctx, scraper.clone());
        }

        // Processa mensagens da UI
        while let Ok(message) = self.ui_rx.try_recv() {
            match message {
                UIMessage::UpdateTestResults(results) => {
                    self.state.test_results = results;
                }
                UIMessage::UpdateErrorMessage(msg) => {
                    self.state.error_message = msg;
                }
            }
        }

        // Exibir resultados do teste
        if !self.state.test_results.is_empty() {
            ui.separator();
            ui.heading("✅ Resultados do Teste:");
            for result in &self.state.test_results {
                ui.label(&result.title);
                ui.hyperlink_to("Abrir", &result.url);
                ui.separator();
            }
        }

        // Exibir erros
        if !self.state.error_message.is_empty() {
            ui.colored_label(Color32::RED, &self.state.error_message);
        }

        // Botão para voltar ao menu
        if ui.button("🔙 Voltar ao Menu").clicked() {
            self.state = DevModeState::default();
        }
    }

    pub fn add_new_site(&mut self, scraper: Arc<TokioMutex<WebScraper>>) -> Result<(), String> {
        // Validação básica
        if self.state.new_site_name.is_empty() ||
            self.state.new_site_base_url.is_empty() ||
            self.state.new_site_search_path.is_empty() ||
            self.state.new_site_video_selector.is_empty() ||
            self.state.new_site_title_selector.is_empty() {
            self.state.error_message = "Preencha todos os campos obrigatórios!".to_string();
            return Err("Campos obrigatórios não preenchidos".to_string());
        }

        // Cria a configuração do novo site
        let new_site_config = SiteConfig {
            base_url: self.state.new_site_base_url.clone(),
            search_path: self.state.new_site_search_path.clone(),
            video_selector: self.state.new_site_video_selector.clone(),
            title_selector: self.state.new_site_title_selector.clone(),
            thumbnail_selector: self.state.new_site_thumbnail_selector.clone(),
            duration_selector: self.state.new_site_duration_selector.clone(),
            url_transform: self.state.new_site_url_transform.clone(),
            recommendations_selector: self.state.new_site_recommendations_selector.clone(),
        };

        // Adiciona ao scraper
        scraper.lock().unwrap().site_configs.insert(
            self.state.new_site_name.clone(),
            new_site_config,
        );

        self.state.error_message.clear();
        self.state = DevModeState::default(); // Limpa o formulário
        Ok(())
    }

    pub fn test_selectors(&mut self, ctx: &Context, scraper: Arc<TokioMutex<WebScraper>>) {
        self.state.error_message.clear();
        self.state.test_results.clear();

        if self.state.test_url.is_empty() {
            self.state.error_message = "Informe uma URL para teste!".to_string();
            return;
        }

        let tx = self.ui_tx.clone();
        let test_config = SiteConfig {
            base_url: self.state.new_site_base_url.clone(),
            search_path: String::new(),
            video_selector: self.state.new_site_video_selector.clone(),
            title_selector: self.state.new_site_title_selector.clone(),
            thumbnail_selector: self.state.new_site_thumbnail_selector.clone(),
            duration_selector: self.state.new_site_duration_selector.clone(),
            url_transform: self.state.new_site_url_transform.clone(),
            recommendations_selector: None,
        };
        let test_url = self.state.test_url.clone();

        tokio::spawn(async move {
            let scraper = scraper.lock().await;
            let html = match scraper.fetch_with_retry(&test_url, 3).await {
                Ok(html) => html,
                Err(e) => {
                    tx.send(UIMessage::UpdateErrorMessage(format!("Erro ao buscar URL: {}", e))).unwrap();
                    return;
                }
            };

            let document = Html::parse_document(&html);
            let video_selector = match Selector::parse(&test_config.video_selector) {
                Ok(s) => s,
                Err(e) => {
                    tx.send(UIMessage::UpdateErrorMessage(format!("Seletor de vídeo inválido: {}", e))).unwrap();
                    return;
                }
            };

            let title_selector = match Selector::parse(&test_config.title_selector) {
                Ok(s) => s,
                Err(e) => {
                    tx.send(UIMessage::UpdateErrorMessage(format!("Seletor de título inválido: {}", e))).unwrap();
                    return;
                }
            };

            let mut results = Vec::new();
            for element in document.select(&video_selector).take(5) {
                let href = element.value().attr("href").unwrap_or("").trim().to_string();
                if href.is_empty() {
                    continue;
                }

                let url = if let Some(transform) = &test_config.url_transform {
                    if href.starts_with('/') {
                        format!("{}{}", test_config.base_url, href)
                    } else {
                        href.to_string()
                    }
                } else {
                    href.to_string()
                };

                let title = element
                    .select(&title_selector)
                    .next()
                    .map(|e| e.text().collect::<String>().trim().to_string())
                    .unwrap_or_default();

                if !url.is_empty() && !title.is_empty() {
                    results.push(VideoResult {
                        title,
                        url,
                        thumbnail: None,
                        duration: None,
                        site: "test".to_string(),
                    });
                }
            }

            if results.is_empty() {
                tx.send(UIMessage::UpdateErrorMessage("Nenhum resultado encontrado. Verifique os seletores!".to_string())).unwrap();
            } else {
                tx.send(UIMessage::UpdateTestResults(results)).unwrap();
            }
        });
    }
}
