#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

mod app;
mod models;
mod sync;
mod utils;

use app::SyncApp;
use eframe::egui;
use image::{ImageBuffer, Rgba};

// Embed the font directly into the binary to ensure portability.
const FONT_MSYH: &[u8] = include_bytes!("../assets/msyh.ttc");

fn main() -> Result<(), eframe::Error> {
    let icon = create_icon();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0]) // Increased default size
            .with_icon(egui::IconData {
                rgba: icon.into_raw(),
                width: 64,
                height: 64,
            }),
        ..Default::default()
    };

    eframe::run_native(
        "SyncU",
        options,
        Box::new(|cc| {
            // --- Font and Style Configuration ---
            let mut fonts = egui::FontDefinitions::default();

            // Load the embedded font
            fonts.font_data.insert(
                "chinese_font".to_owned(),
                egui::FontData::from_static(FONT_MSYH).into(),
            );

            // Set the custom font as the first fallback for proportional text
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "chinese_font".to_owned());

            // Set the custom font as the first fallback for monospace text
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .insert(0, "chinese_font".to_owned());

            cc.egui_ctx.set_fonts(fonts);

            // --- Custom Visuals ---
            let mut style = (*cc.egui_ctx.style()).clone();
            style.text_styles = [
                (
                    egui::TextStyle::Heading,
                    egui::FontId::new(24.0, egui::FontFamily::Proportional),
                ),
                (
                    egui::TextStyle::Body,
                    egui::FontId::new(16.0, egui::FontFamily::Proportional),
                ),
                (
                    egui::TextStyle::Button,
                    egui::FontId::new(16.0, egui::FontFamily::Proportional),
                ),
                (
                    egui::TextStyle::Small,
                    egui::FontId::new(12.0, egui::FontFamily::Proportional),
                ),
                (
                    egui::TextStyle::Monospace,
                    egui::FontId::new(14.0, egui::FontFamily::Monospace),
                ),
            ]
            .into();

            // A more modern, clean look
            style.visuals = egui::Visuals {
                widgets: egui::style::Widgets::light(),
                override_text_color: Some(egui::Color32::from_rgb(30, 30, 30)),
                ..egui::Visuals::light()
            };
            // Set rounding for all widget states
            let corner_radius = egui::epaint::CornerRadius::same(5u8);
            style.visuals.widgets.noninteractive.corner_radius = corner_radius;
            style.visuals.widgets.inactive.corner_radius = corner_radius;
            style.visuals.widgets.hovered.corner_radius = corner_radius;
            style.visuals.widgets.active.corner_radius = corner_radius;

            style.visuals.widgets.noninteractive.bg_fill = egui::Color32::from_gray(248);
            style.visuals.widgets.inactive.bg_fill = egui::Color32::from_gray(220);
            style.visuals.widgets.hovered.bg_fill = egui::Color32::from_gray(200);
            style.visuals.widgets.active.bg_fill = egui::Color32::from_gray(180);
            style.visuals.selection.bg_fill = egui::Color32::from_rgb(0, 120, 215);

            cc.egui_ctx.set_style(style);

            Ok(Box::new(SyncApp::new(cc.egui_ctx.clone())))
        }),
    )
}

// Generates the application icon programmatically.
fn create_icon() -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    let mut image = ImageBuffer::new(64, 64);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let border = 2;
        let is_border = x < border || x >= 64 - border || y < border || y >= 64 - border;
        
        // A simple, modern "S" shape for "SyncU"
        let is_s_shape = 
            (x > 16 && x < 48 && y > 16 && y < 24) || // Top bar
            (x > 16 && x < 32 && y > 24 && y < 32) || // Top-left vertical
            (x > 16 && x < 48 && y > 32 && y < 40) || // Middle bar
            (x > 32 && x < 48 && y > 40 && y < 48) || // Bottom-right vertical
            (x > 16 && x < 48 && y > 48 && y < 56);   // Bottom bar

        if is_border {
            *pixel = Rgba([0, 120, 215, 255]); // Blue border
        } else if is_s_shape {
            *pixel = Rgba([0, 120, 215, 255]); // Blue 'S'
        } else {
            *pixel = Rgba([255, 255, 255, 255]); // White background
        }
    }
    image
}