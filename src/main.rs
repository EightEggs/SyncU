#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

use chrono::Local;
use eframe::egui;
use image::{ImageBuffer, Rgba};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Instant, SystemTime};
use sysinfo::{DiskExt, System, SystemExt};
use walkdir::WalkDir;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const LARGE_FILE_THRESHOLD: u64 = 10 * 1024 * 1024; // 10 MB

#[derive(Clone)]
enum SyncMessage {
    Log(String),
    ConfirmDeletion(PathBuf),
    DeletionConfirmed(bool),
    Progress(f32, String),
    Complete,
    Stop,
    Stopped,
}

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
                (egui::TextStyle::Heading, egui::FontId::new(24.0, egui::FontFamily::Proportional)),
                (egui::TextStyle::Body, egui::FontId::new(16.0, egui::FontFamily::Proportional)),
                (egui::TextStyle::Button, egui::FontId::new(16.0, egui::FontFamily::Proportional)),
                (egui::TextStyle::Small, egui::FontId::new(12.0, egui::FontFamily::Proportional)),
            ].into();
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
        let is_u_shape = (x > 15 && x < 48 && y > 15 && y < 48) && !(x > 18 && x < 45 && y > 18 && y < 45);
        
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

#[derive(Serialize, Deserialize, Debug, Clone)]
struct FileInfo {
    path: PathBuf,
    hash: String,
    modified: SystemTime,
    #[serde(default)]
    size: u64,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
struct SyncData {
    files: HashMap<PathBuf, FileInfo>,
}

#[derive(Debug)]
enum SyncAction {
    Upload(PathBuf),
    Download(PathBuf),
    DeleteLocal(PathBuf),
    DeleteRemote(PathBuf),
    Conflict(PathBuf),
}

struct SyncApp {
    local_folder: Option<PathBuf>,
    usb_drives: Vec<PathBuf>,
    selected_usb_drive: Option<PathBuf>,
    sync_log: Vec<String>,
    syncing: bool,
    stopping: bool,
    show_confirmation: bool,
    show_about_window: bool,
    file_to_delete: Option<PathBuf>,
    progress: f32,
    current_file: String,
    tx_to_sync: Sender<SyncMessage>,
    rx_from_sync: Receiver<SyncMessage>,
    ctx: egui::Context,
}

impl SyncApp {
    fn new(ctx: egui::Context) -> Self {
        let (tx_to_sync, rx_from_ui) = channel();
        let (tx_from_sync, rx_from_sync) = channel();

        let sync_thread_tx = tx_from_sync.clone();
        thread::spawn(move || {
            while let Ok(msg) = rx_from_ui.recv() {
                if let SyncMessage::Log(local_folder_str) = msg {
                    let local_folder = Some(PathBuf::from(local_folder_str));
                    if let Ok(SyncMessage::Log(usb_drive_str)) = rx_from_ui.recv() {
                        let usb_drive = Some(PathBuf::from(usb_drive_str));
                        run_sync(local_folder, usb_drive, sync_thread_tx.clone(), &rx_from_ui);
                    }
                }
            }
        });

        let usb_drives = find_usb_drives();
        let selected_usb_drive = if usb_drives.len() == 1 { Some(usb_drives[0].clone()) } else { None };

        Self {
            local_folder: None,
            usb_drives,
            selected_usb_drive,
            sync_log: vec!["准备就绪".to_owned()],
            syncing: false,
            stopping: false,
            show_confirmation: false,
            show_about_window: false,
            file_to_delete: None,
            progress: 0.0,
            current_file: "".to_owned(),
            tx_to_sync,
            rx_from_sync,
            ctx,
        }
    }
}

impl eframe::App for SyncApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Ok(msg) = self.rx_from_sync.try_recv() {
            match msg {
                SyncMessage::Log(log) => self.sync_log.push(log),
                SyncMessage::ConfirmDeletion(path) => {
                    self.show_confirmation = true;
                    self.file_to_delete = Some(path);
                }
                SyncMessage::Progress(progress, file) => {
                    self.progress = progress;
                    self.current_file = file;
                }
                SyncMessage::Complete => {
                    self.syncing = false;
                    self.stopping = false;
                }
                SyncMessage::Stopped => {
                    self.syncing = false;
                    self.stopping = false;
                }
                _ => {}
            }
            self.ctx.request_repaint();
        }

        if self.show_confirmation {
            egui::Window::new("确认删除")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    if let Some(file_to_delete) = &self.file_to_delete {
                        ui.label(format!("您确定要删除文件 {:?}吗?", file_to_delete));
                    }
                    ui.horizontal(|ui| {
                        if ui.button("确认").clicked() {
                            self.tx_to_sync.send(SyncMessage::DeletionConfirmed(true)).unwrap_or_default();
                            self.show_confirmation = false;
                        }
                        if ui.button("取消").clicked() {
                            self.tx_to_sync.send(SyncMessage::DeletionConfirmed(false)).unwrap_or_default();
                            self.show_confirmation = false;
                        }
                    });
                });
        }

        if self.show_about_window {
            egui::Window::new("关于 SyncU")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.heading("SyncU");
                        ui.label(format!("版本: {}", APP_VERSION));
                        ui.label("作者: Eight_Eggs");
                        ui.hyperlink_to("访问 GitHub 仓库", "https://github.com/EightEggs/SyncU");
                        if ui.button("关闭").clicked() {
                            self.show_about_window = false;
                        }
                    });
                });
        }

        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            if self.syncing {
                ui.horizontal(|ui| {
                    ui.add(egui::ProgressBar::new(self.progress).show_percentage().desired_width(200.0));
                    ui.label(&self.current_file);
                });
            } else {
                ui.label("准备就绪");
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("SyncU: Sync Your Files with USB");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("关于").clicked() {
                        self.show_about_window = true;
                    }
                });
            });
            ui.add_space(10.0);

            ui.add_enabled_ui(!self.syncing, |ui| {
                egui::Grid::new("path_grid")
                    .num_columns(3)
                    .spacing([10.0, 10.0])
                    .show(ui, |ui| {
                        ui.label("本地文件夹:");
                        let local_path_text = self.local_folder.as_ref().map_or_else(
                            || "未选择".to_owned(),
                            |p| p.display().to_string(),
                        );
                        ui.label(&local_path_text);
                        if ui.button("选择...").clicked() {
                            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                self.local_folder = Some(path);
                            }
                        }
                        ui.end_row();

                        ui.label("U盘:");
                        if self.usb_drives.len() > 1 {
                            egui::ComboBox::from_label("")
                                .selected_text(self.selected_usb_drive.as_ref().map_or("请选择U盘", |p| p.to_str().unwrap_or_default()))
                                .show_ui(ui, |ui| {
                                    for drive in &self.usb_drives {
                                        ui.selectable_value(&mut self.selected_usb_drive, Some(drive.clone()), drive.to_str().unwrap_or_default());
                                    }
                                });
                        } else {
                            let usb_path_text = self.selected_usb_drive.as_ref().map_or_else(
                                || "未检测到".to_owned(),
                                |p| p.display().to_string(),
                            );
                            ui.label(&usb_path_text);
                        }
                        if ui.button("刷新").clicked() {
                            self.usb_drives = find_usb_drives();
                            self.selected_usb_drive = if self.usb_drives.len() == 1 { Some(self.usb_drives[0].clone()) } else { None };
                        }
                        ui.end_row();
                    });
            });

            ui.separator();
            ui.add_space(10.0);

            ui.scope(|ui| {
                ui.set_min_height(40.0);
                ui.vertical_centered_justified(|ui| {
                    if self.syncing {
                        if self.stopping {
                            let stop_button = egui::Button::new(
                                egui::RichText::new("正在停止...").color(egui::Color32::WHITE),
                            )
                            .fill(egui::Color32::from_rgb(150, 0, 0))
                            .min_size(egui::vec2(120.0, 40.0));
                            ui.add_enabled(false, stop_button);
                        } else {
                            let stop_button = egui::Button::new(
                                egui::RichText::new("停止同步").color(egui::Color32::WHITE),
                            )
                            .fill(egui::Color32::from_rgb(200, 0, 0))
                            .min_size(egui::vec2(120.0, 40.0));
                            if ui.add(stop_button).clicked() {
                                self.stopping = true;
                                self.tx_to_sync.send(SyncMessage::Stop).unwrap_or_default();
                            }
                        }
                    } else {
                        let enabled = self.local_folder.is_some() && self.selected_usb_drive.is_some();
                        if ui.add_enabled(enabled, egui::Button::new("立即同步").min_size(egui::vec2(120.0, 40.0))).clicked() {
                            self.syncing = true;
                            self.sync_log = vec!["同步中...".to_owned()];
                            if let (Some(local_folder), Some(selected_usb_drive)) = (self.local_folder.as_ref(), self.selected_usb_drive.as_ref()) {
                                self.tx_to_sync.send(SyncMessage::Log(local_folder.to_str().unwrap_or_default().to_string())).unwrap_or_default();
                                self.tx_to_sync.send(SyncMessage::Log(selected_usb_drive.to_str().unwrap_or_default().to_string())).unwrap_or_default();
                            }
                        }
                    }
                });
            });

            ui.add_space(10.0);
            ui.separator();

            egui::Frame::canvas(ui.style()).show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(self.sync_log.join("
"));
                });
            });
        });
    }
}

fn run_sync(
    local_folder: Option<PathBuf>,
    usb_drive: Option<PathBuf>,
    tx: Sender<SyncMessage>,
    rx: &Receiver<SyncMessage>,
) {
    let was_stopped = match (|| -> Result<bool, Box<dyn std::error::Error>> {
        let local_path = local_folder.as_ref().ok_or("未选择本地文件夹")?;
        let usb_root_path = usb_drive.as_ref().ok_or("未检测到U盘")?;

        let sync_folder_name = local_path.file_name().ok_or("无效的本地文件夹名称")?;
        let usb_sync_path = usb_root_path.join(sync_folder_name);
        fs::create_dir_all(&usb_sync_path)?;

        let metadata_path = usb_sync_path.join(".syncu_metadata.json");

        tx.send(SyncMessage::Progress(0.0, "正在加载上次同步记录...".to_string()))?;
        let last_sync_data = load_sync_data(&metadata_path)?;

        // Scan local directory with progress
        tx.send(SyncMessage::Progress(0.0, "正在统计本地文件...".to_string()))?;
        let local_total = WalkDir::new(local_path).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()).count();
        let local_sync_data = match scan_directory_with_progress(local_path, &tx, rx, local_total, "扫描本地")? {
            Some(data) => data,
            None => return Ok(true), // Stopped
        };

        // Scan remote directory with progress
        tx.send(SyncMessage::Progress(0.0, "正在统计U盘文件...".to_string()))?;
        let remote_total = WalkDir::new(&usb_sync_path).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()).count();
        let remote_sync_data = match scan_directory_with_progress(&usb_sync_path, &tx, rx, remote_total, "扫描U盘")? {
            Some(data) => data,
            None => return Ok(true), // Stopped
        };

        let mut sync_plan = Vec::new();

        let all_files: Vec<_> = local_sync_data
            .files
            .keys()
            .chain(remote_sync_data.files.keys())
            .chain(last_sync_data.files.keys())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .cloned()
            .collect();

        tx.send(SyncMessage::Progress(0.0, "正在分析文件差异...".to_string()))?;
        for relative_path in all_files.iter() {
            if let Ok(SyncMessage::Stop) = rx.try_recv() {
                return Ok(true);
            }

            let local_file = local_sync_data.files.get(relative_path);
            let remote_file = remote_sync_data.files.get(relative_path);
            let last_sync_file = last_sync_data.files.get(relative_path);

            match (local_file, remote_file, last_sync_file) {
                (Some(local), Some(remote), Some(last)) => {
                    if local.modified > last.modified && local.size != last.size {
                        sync_plan.push(SyncAction::Upload(relative_path.clone()));
                    } else if remote.modified > last.modified && remote.size != last.size {
                        sync_plan.push(SyncAction::Download(relative_path.clone()));
                    } else if local.modified > last.modified && remote.modified > last.modified {
                        sync_plan.push(SyncAction::Conflict(relative_path.clone()));
                    }
                }
                (Some(_local), None, None) => sync_plan.push(SyncAction::Upload(relative_path.clone())),
                (None, Some(_remote), None) => sync_plan.push(SyncAction::Download(relative_path.clone())),
                (None, Some(_remote), Some(_last)) => sync_plan.push(SyncAction::DeleteRemote(relative_path.clone())),
                (Some(_local), None, Some(_last)) => sync_plan.push(SyncAction::DeleteLocal(relative_path.clone())),
                _ => {}
            }
        }

        let total_sync_size = sync_plan.iter().try_fold(0u64, |acc, action| -> Result<u64, Box<dyn std::error::Error>> {
            Ok(acc + match action {
                SyncAction::Upload(path) => fs::metadata(local_path.join(path))?.len(),
                SyncAction::Download(path) => fs::metadata(usb_sync_path.join(path))?.len(),
                SyncAction::Conflict(path) => fs::metadata(local_path.join(path))?.len(),
                _ => 0,
            })
        })?;

        let mut processed_size = 0u64;

        if sync_plan.is_empty() {
            tx.send(SyncMessage::Log("同步完成! 未检测到变化.".to_owned()))?;
        }

        for action in sync_plan {
            if let Ok(SyncMessage::Stop) = rx.try_recv() {
                return Ok(true);
            }

            let file_size = match &action {
                SyncAction::Upload(path) => fs::metadata(local_path.join(path))?.len(),
                SyncAction::Download(path) => fs::metadata(usb_sync_path.join(path))?.len(),
                SyncAction::Conflict(path) => fs::metadata(local_path.join(path))?.len(),
                _ => 0,
            };

            let current_file_name = match &action {
                SyncAction::Upload(path) | SyncAction::Download(path) | SyncAction::Conflict(path) => path.to_str().unwrap_or_default().to_string(),
                SyncAction::DeleteLocal(path) | SyncAction::DeleteRemote(path) => format!("删除: {}", path.to_str().unwrap_or_default()),
            };

            let message = match action {
                SyncAction::Upload(path) => {
                    let from = local_path.join(&path);
                    let to = usb_sync_path.join(&path);
                    if file_size > LARGE_FILE_THRESHOLD {
                        if copy_large_file_with_progress(&from, &to, &current_file_name, &tx, rx, total_sync_size, processed_size)? {
                            return Ok(true); // Stopped
                        }
                    } else {
                        fs::copy(&from, &to)?;
                    }
                    format!("[{}] 上传: 本地 -> U盘: {}", Local::now().format("%Y-%m-%d %H:%M:%S"), path.display())
                }
                SyncAction::Download(path) => {
                    let from = usb_sync_path.join(&path);
                    let to = local_path.join(&path);
                     if file_size > LARGE_FILE_THRESHOLD {
                        if copy_large_file_with_progress(&from, &to, &current_file_name, &tx, rx, total_sync_size, processed_size)? {
                            return Ok(true); // Stopped
                        }
                    } else {
                        fs::copy(&from, &to)?;
                    }
                    format!("[{}] 下载: U盘 -> 本地: {}", Local::now().format("%Y-%m-%d %H:%M:%S"), path.display())
                }
                SyncAction::DeleteRemote(path) => {
                    let absolute_path = usb_sync_path.join(&path);
                    tx.send(SyncMessage::ConfirmDeletion(absolute_path.clone()))?;
                    let confirmed = loop {
                        match rx.recv()? {
                            SyncMessage::DeletionConfirmed(c) => break c,
                            SyncMessage::Stop => return Ok(true),
                            _ => {}
                        }
                    };
                    if confirmed {
                        if absolute_path.exists() {
                            fs::remove_file(absolute_path)?;
                        }
                        format!("[{}] 删除: U盘: {}", Local::now().format("%Y-%m-%d %H:%M:%S"), path.display())
                    } else {
                        format!("[{}] 取消删除: U盘: {}", Local::now().format("%Y-%m-%d %H:%M:%S"), path.display())
                    }
                }
                SyncAction::DeleteLocal(path) => {
                    let absolute_path = local_path.join(&path);
                    tx.send(SyncMessage::ConfirmDeletion(absolute_path.clone()))?;
                    let confirmed = loop {
                        match rx.recv()? {
                            SyncMessage::DeletionConfirmed(c) => break c,
                            SyncMessage::Stop => return Ok(true),
                            _ => {}
                        }
                    };
                    if confirmed {
                        if absolute_path.exists() {
                            fs::remove_file(absolute_path)?;
                        }
                        format!("[{}] 删除: 本地: {}", Local::now().format("%Y-%m-%d %H:%M:%S"), path.display())
                    } else {
                        format!("[{}] 取消删除: 本地: {}", Local::now().format("%Y-%m-%d %H:%M:%S"), path.display())
                    }
                }
                SyncAction::Conflict(path) => {
                    let local_file_path = local_path.join(&path);
                    let conflict_path = local_file_path.with_extension(
                        format!("{}.conflict", local_file_path.extension().unwrap_or_default().to_str().unwrap_or(""))
                    );
                    fs::rename(&local_file_path, &conflict_path)?;
                    let from = usb_sync_path.join(&path);
                    let to = local_path.join(&path);
                    if file_size > LARGE_FILE_THRESHOLD {
                        if copy_large_file_with_progress(&from, &to, &current_file_name, &tx, rx, total_sync_size, processed_size)? {
                            return Ok(true); // Stopped
                        }
                    } else {
                        fs::copy(&from, &to)?;
                    }
                    format!(
                        "[{}] 冲突: {} 已备份至 {}",
                        Local::now().format("%Y-%m-%d %H:%M:%S"),
                        path.display(),
                        conflict_path.display()
                    )
                }
            };
            processed_size += file_size;
            if total_sync_size > 0 {
                 tx.send(SyncMessage::Progress(processed_size as f32 / total_sync_size as f32, current_file_name))?;
            }
            tx.send(SyncMessage::Log(message.clone()))?;
            write_log_entry(&message, &usb_sync_path)?;
        }

        let final_sync_data = scan_directory_with_progress(local_path, &tx, rx, local_total, "更新本地元数据")?;
        if let Some(final_sync_data) = final_sync_data {
            save_sync_data(&final_sync_data, &metadata_path)?;
        }

        tx.send(SyncMessage::Progress(1.0, "同步完成!".to_string()))?;
        Ok(false)
    })() {
        Ok(stopped) => stopped,
        Err(e) => {
            let msg = format!("错误: {}", e);
            tx.send(SyncMessage::Log(msg.clone())).unwrap_or_default();
            if let (Some(local_folder), Some(usb_drive)) = (local_folder, usb_drive) {
                let sync_folder_name = local_folder.file_name().unwrap();
                let usb_sync_path = usb_drive.join(sync_folder_name);
                write_log_entry(&msg, &usb_sync_path).ok();
            }
            false
        }
    };

    if was_stopped {
        let msg = format!("[{}] 同步已由用户停止。", Local::now().format("%Y-%m-%d %H:%M:%S"));
        tx.send(SyncMessage::Log(msg)).unwrap_or_default();
        tx.send(SyncMessage::Stopped).unwrap_or_default();
    } else {
        tx.send(SyncMessage::Complete).unwrap_or_default();
    }
}

fn find_usb_drives() -> Vec<PathBuf> {
    let mut sys = System::new_all();
    sys.refresh_disks_list();
    sys.disks()
        .iter()
        .filter(|d| d.is_removable())
        .map(|d| d.mount_point().to_path_buf())
        .collect()
}

fn scan_directory_with_progress(
    base_path: &Path,
    tx: &Sender<SyncMessage>,
    rx: &Receiver<SyncMessage>,
    total_files: usize,
    ui_message_prefix: &str,
) -> Result<Option<SyncData>, Box<dyn std::error::Error>> {
    let mut files = HashMap::new();
    let walker = WalkDir::new(base_path).into_iter();
    let mut processed_files = 0;

    for entry in walker.filter_map(|e| e.ok()) {
        if let Ok(SyncMessage::Stop) = rx.try_recv() {
            return Ok(None); // Stopped
        }

        if entry.file_type().is_file() {
            processed_files += 1;
            let path = entry.path().to_path_buf();
            if path.file_name().unwrap_or_default() == ".syncu_metadata.json"
                || path.file_name().unwrap_or_default() == "SyncU.log"
            {
                continue;
            }

            let progress = if total_files > 0 { processed_files as f32 / total_files as f32 } else { 1.0 };
            let file_name = entry.path().file_name().unwrap_or_default().to_str().unwrap_or_default();
            tx.send(SyncMessage::Progress(progress, format!("{} ({}/{}) - {}", ui_message_prefix, processed_files, total_files, file_name)))?;

            let metadata = fs::metadata(&path)?;
            let modified = metadata.modified()?;
            let size = metadata.len();
            let hash = if metadata.len() < 1_000_000 { // Only hash small files
                let content = fs::read(&path)?;
                format!("{:x}", Sha256::digest(&content))
            } else {
                String::new()
            };

            let relative_path = path.strip_prefix(base_path)?.to_path_buf();

            files.insert(
                relative_path.clone(),
                FileInfo {
                    path: relative_path,
                    hash,
                    modified,
                    size,
                },
            );
        }
    }
    Ok(Some(SyncData { files }))
}

fn save_sync_data(sync_data: &SyncData, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_string_pretty(sync_data)?;
    fs::write(path, json)?;
    Ok(())
}

fn load_sync_data(path: &Path) -> Result<SyncData, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Ok(SyncData::default());
    }
    let json = fs::read_to_string(path)?;
    let sync_data = serde_json::from_str(&json)?;
    Ok(sync_data)
}

fn copy_large_file_with_progress(
    from: &Path,
    to: &Path,
    file_name_for_ui: &str,
    tx: &Sender<SyncMessage>,
    rx: &Receiver<SyncMessage>,
    total_sync_size: u64,
    processed_size_before: u64,
) -> Result<bool, Box<dyn std::error::Error>> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut source = File::open(from)?;
    let mut dest = File::create(to)?;
    let file_size = source.metadata()?.len();

    const BUFFER_SIZE: usize = 4 * 1024 * 1024; // 4MB
    let mut buffer = vec![0; BUFFER_SIZE];
    let mut bytes_copied: u64 = 0;
    let mut last_update = Instant::now();

    loop {
        if let Ok(SyncMessage::Stop) = rx.try_recv() {
            drop(dest);
            fs::remove_file(to)?;
            return Ok(true);
        }

        let bytes_read = source.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        dest.write_all(&buffer[..bytes_read])?;
        bytes_copied += bytes_read as u64;

        if total_sync_size > 0 && (last_update.elapsed().as_millis() > 100 || bytes_copied == file_size) {
            let progress = (processed_size_before + bytes_copied) as f32 / total_sync_size as f32;
            tx.send(SyncMessage::Progress(progress, file_name_for_ui.to_string()))?;
            last_update = Instant::now();
        }
    }

    Ok(false)
}

fn write_log_entry(entry: &str, usb_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let log_path = usb_path.join("SyncU.log");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    writeln!(file, "{}", entry)?;
    Ok(())
}
