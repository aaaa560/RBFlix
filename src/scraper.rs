use scraper::{Html, Selector};
use reqwest;
use std::collections::HashMap;
use crate::video::VideoResult;

// Config de sites
#[derive(Debug, Clone)]
pub struct SiteConfig {
    pub base_url: String,
    pub search_path: String,
    pub video_selector: String,
    pub title_selector: String,
    pub thumbnail_selector: Option<String>,
    pub duration_selector: Option<String>,
    pub url_transform: Option<fn(&str) -> String>,
    pub recommendations_path: Option<String>,
    pub recommendations_selector: Option<String>,
}

// Scraper
pub struct WebScraper {
    pub client: reqwest::Client,
    pub site_configs: HashMap<String, SiteConfig>,
}

// Implementação do scraper
impl WebScraper {
    pub fn new() -> Self {
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

    pub async fn search_videos(
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

    pub async fn search_recommendations(
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

    pub async fn fetch_with_retry(&self, url: &str, max_retries: u32) -> Result<String, String> {
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

    pub fn clean_title(&self, title: &str) -> String {
        title
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace() || "()[]{}.,!?-_".contains(*c))
            .collect::<String>()
            .trim()
            .chars()
            .take(80)
            .collect()
    }

    pub async fn download_thumbnail(&self, url: &str) -> Result<Vec<u8>, String> {
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