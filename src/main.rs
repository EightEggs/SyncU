#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

mod app;
mod models;
mod sync;
mod utils;

use app::SyncApp;
use eframe::egui;
use image::{ImageBuffer, Rgba};

fn main() {
    let icon = create_icon();
    let options = eframe::NativeOptions {
        initial_window_size: Some(egui::vec2(600.0, 400.0)),
        icon_data: Some(eframe::IconData {
            rgba: icon.into_raw(),
            width: 64,
            height: 64,
        }),
        ..Default::default()
    };
    let _ = eframe::run_native(
        "SyncU",
        options,
        Box::new(|cc| {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "chinese_font".to_owned(),
                egui::FontData::from_static(include_bytes!("C:/Windows/Fonts/msyh.ttc")),
            );
            fonts
                .families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .insert(0, "chinese_font".to_owned());
            cc.egui_ctx.set_fonts(fonts);

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
            ]
            .into();
            style.visuals = egui::Visuals {
                window_rounding: 5.0.into(),
                widgets: egui::style::Widgets::light(),
                override_text_color: Some(egui::Color32::from_rgb(30, 30, 30)),
                ..egui::Visuals::light()
            };
            style.visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(240, 240, 240);
            style.visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(220, 220, 220);
            style.visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(200, 200, 200);
            style.visuals.widgets.active.bg_fill = egui::Color32::from_rgb(180, 180, 180);
            style.visuals.selection.bg_fill = egui::Color32::from_rgb(0, 120, 215);
            cc.egui_ctx.set_style(style);

            Box::new(SyncApp::new(cc.egui_ctx.clone()))
        }),
    );
}

fn create_icon() -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    let mut image = ImageBuffer::new(64, 64);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let is_border = x < 2 || x > 61 || y < 2 || y > 61;
        let is_u_shape =
            (x > 15 && x < 48 && y > 15 && y < 48) && !(x > 18 && x < 45 && y > 18 && y < 45);

        if is_border {
            *pixel = Rgba([0, 120, 215, 255]); // Blue border
        } else if is_u_shape {
            *pixel = Rgba([0, 120, 215, 255]); // Blue 'U'
        } else {
            *pixel = Rgba([255, 255, 255, 255]); // White background
        }
    }
    image
}
