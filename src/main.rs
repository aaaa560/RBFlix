mod app;
mod scraper;
mod settings;
mod user;
mod video;
mod dev_mode;
mod encrypt;

use app::RambleyFlixApp;
use eframe::egui;
use gstreamer::prelude::*;
use serde::{Deserialize, Serialize};
use std::io::Write;

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
