#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

mod app;
mod models;
mod sync;
mod utils;

use app::SyncApp;
use eframe::egui;
use image::{ImageBuffer, Rgba};
use models::Theme;

// Embed the font directly into the binary to ensure portability.
const FONT_MSYH: &[u8] = include_bytes!("../assets/msyh.ttc");

fn main() -> Result<(), eframe::Error> {
    let icon = create_icon();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
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
            let app = SyncApp::new(cc.egui_ctx.clone());
            setup_fonts(&cc.egui_ctx);
            apply_theme(&cc.egui_ctx, &app.current_theme);
            Ok(Box::new(app))
        }),
    )
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "chinese_font".to_owned(),
        egui::FontData::from_static(FONT_MSYH).into(),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "chinese_font".to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "chinese_font".to_owned());
    ctx.set_fonts(fonts);
}

pub fn apply_theme(ctx: &egui::Context, theme: &Theme) {
    let visuals = match theme {
        Theme::Light => egui::Visuals::light(),
        Theme::Dark => egui::Visuals::dark(),
    };

    let mut style = (*ctx.style()).clone();
    style.visuals = visuals;

    // --- Custom text styles ---
    style.text_styles = [
        (
            egui::TextStyle::Heading,
            egui::FontId::new(22.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Body,
            egui::FontId::new(14.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Button,
            egui::FontId::new(14.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Small,
            egui::FontId::new(11.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Monospace,
            egui::FontId::new(12.0, egui::FontFamily::Monospace),
        ),
    ]
    .into();

    // --- Custom widget styling ---
    let large_corner_radius = egui::CornerRadius::from(8.0);
    let small_corner_radius = egui::CornerRadius::from(4.0);

    style.visuals.widgets.noninteractive.corner_radius = large_corner_radius;
    style.visuals.widgets.inactive.corner_radius = large_corner_radius;
    style.visuals.widgets.hovered.corner_radius = large_corner_radius;
    style.visuals.widgets.active.corner_radius = large_corner_radius;
    style.visuals.widgets.open.corner_radius = large_corner_radius;

    style.visuals.window_corner_radius = large_corner_radius;
    style.visuals.menu_corner_radius = small_corner_radius;
    style.visuals.popup_shadow = egui::epaint::Shadow::NONE;

    style.spacing.item_spacing = egui::Vec2::new(8.0, 6.0);
    style.spacing.button_padding = egui::Vec2::new(12.0, 6.0);

    ctx.set_style(style);
}

// Generates a modern application icon programmatically.
fn create_icon() -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    let mut image = ImageBuffer::new(64, 64);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let background_value = 245;
        *pixel = Rgba([background_value, background_value, background_value, 255]);
        
        let center_x = x as f32 - 32.0;
        let center_y = y as f32 - 32.0;
        let distance_from_center = (center_x * center_x + center_y * center_y).sqrt();
        
        if distance_from_center < 28.0 {
            if (x > 18 && x < 46) && 
               ((y > 18 && y < 24) ||
                (x > 18 && x < 26 && y > 24 && y < 32) ||
                (y > 32 && y < 38) ||
                (x > 38 && x < 46 && y > 38 && y < 46) ||
                (y > 46 && y < 52)) {
                *pixel = Rgba([0, 120, 215, 255]);
            }
        }
        
        if distance_from_center > 26.0 && distance_from_center < 28.0 {
            *pixel = Rgba([200, 200, 200, 100]);
        }
    }
    image
}