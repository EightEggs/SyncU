#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

use chrono::Local;
use eframe::egui;
use image::{ImageBuffer, Rgba};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::SystemTime;
use sysinfo::{DiskExt, System, SystemExt};
use walkdir::WalkDir;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
enum SyncMessage {
    Log(String),
    ConfirmDeletion(PathBuf),
    DeletionConfirmed(bool),
    Complete,
}

fn main() {
    let icon = create_icon();
    let options = eframe::NativeOptions {
        initial_window_size: Some(egui::vec2(500.0, 400.0)),
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
    show_confirmation: bool,
    file_to_delete: Option<PathBuf>,
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
            show_confirmation: false,
            file_to_delete: None,
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
                SyncMessage::Complete => self.syncing = false,
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

        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("SyncU v{}", APP_VERSION));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label("作者: Eight_Eggs");
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("SyncU - 便捷地在计算机和U盘之间同步文件");
            ui.add_space(10.0);

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

            ui.add_space(10.0);
            ui.scope(|ui| {
                ui.set_min_height(40.0);
                ui.vertical_centered_justified(|ui| {
                    if self.syncing {
                        ui.add(egui::Spinner::new());
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
            ui.separator();
            ui.add_space(10.0);

            egui::Frame::canvas(ui.style()).show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(self.sync_log.join("\n"));
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
    match (|| -> Result<(), Box<dyn std::error::Error>> {
        let local_path = local_folder.as_ref().ok_or("未选择本地文件夹")?;
        let usb_root_path = usb_drive.as_ref().ok_or("未检测到U盘")?;

        let sync_folder_name = local_path.file_name().ok_or("无效的本地文件夹名称")?;
        let usb_sync_path = usb_root_path.join(sync_folder_name);
        fs::create_dir_all(&usb_sync_path)?;

        let metadata_path = usb_sync_path.join(".syncu_metadata.json");

        let last_sync_data = load_sync_data(&metadata_path)?;
        let local_sync_data = scan_directory(local_path)?;
        let remote_sync_data = scan_directory(&usb_sync_path)?;

        let mut sync_plan = Vec::new();

        // Three-way comparison
        let all_files: Vec<_> = local_sync_data
            .files
            .keys()
            .chain(remote_sync_data.files.keys())
            .chain(last_sync_data.files.keys())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .cloned()
            .collect();

        for relative_path in all_files {
            let local_file = local_sync_data.files.get(&relative_path);
            let remote_file = remote_sync_data.files.get(&relative_path);
            let last_sync_file = last_sync_data.files.get(&relative_path);

            match (local_file, remote_file, last_sync_file) {
                // Case 1: File exists everywhere
                (Some(local), Some(remote), Some(last)) => {
                    let local_changed = local.hash != last.hash;
                    let remote_changed = remote.hash != last.hash;

                    if local_changed && !remote_changed {
                        sync_plan.push(SyncAction::Upload(relative_path));
                    } else if !local_changed && remote_changed {
                        sync_plan.push(SyncAction::Download(relative_path));
                    } else if local_changed && remote_changed {
                        if local.hash != remote.hash {
                            sync_plan.push(SyncAction::Conflict(relative_path));
                        }
                    }
                }
                // Case 2: New local file
                (Some(_local), None, None) => {
                    sync_plan.push(SyncAction::Upload(relative_path));
                }
                // Case 3: New remote file
                (None, Some(_remote), None) => {
                    sync_plan.push(SyncAction::Download(relative_path));
                }
                // Case 4: File deleted locally
                (None, Some(_remote), Some(_last)) => {
                    sync_plan.push(SyncAction::DeleteRemote(relative_path));
                }
                // Case 5: File deleted remotely
                (Some(_local), None, Some(_last)) => {
                    sync_plan.push(SyncAction::DeleteLocal(relative_path));
                }
                _ => {}
            }
        }

        let mut log = Vec::new();
        for action in sync_plan {
            let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
            let message = match action {
                SyncAction::Upload(path) => {
                    copy_file(local_path, &usb_sync_path, &path)?;
                    format!("[{}] 上传: 本地 -> U盘: {}", timestamp, path.display())
                }
                SyncAction::Download(path) => {
                    copy_file(&usb_sync_path, local_path, &path)?;
                    format!("[{}] 下载: U盘 -> 本地: {}", timestamp, path.display())
                }
                SyncAction::DeleteRemote(path) => {
                    let absolute_path = usb_sync_path.join(&path);
                    tx.send(SyncMessage::ConfirmDeletion(absolute_path.clone()))?;
                    if let Ok(SyncMessage::DeletionConfirmed(true)) = rx.recv() {
                        if absolute_path.exists() {
                            fs::remove_file(absolute_path)?;
                        }
                        format!("[{}] 删除: U盘: {}", timestamp, path.display())
                    } else {
                        format!("[{}] 取消删除: U盘: {}", timestamp, path.display())
                    }
                }
                SyncAction::DeleteLocal(path) => {
                    let absolute_path = local_path.join(&path);
                    tx.send(SyncMessage::ConfirmDeletion(absolute_path.clone()))?;
                    if let Ok(SyncMessage::DeletionConfirmed(true)) = rx.recv() {
                        if absolute_path.exists() {
                            fs::remove_file(absolute_path)?;
                        }
                        format!("[{}] 删除: 本地: {}", timestamp, path.display())
                    } else {
                        format!("[{}] 取消删除: 本地: {}", timestamp, path.display())
                    }
                }
                SyncAction::Conflict(path) => {
                    let local_file_path = local_path.join(&path);
                    let conflict_path = local_file_path.with_extension(
                        format!("{}.conflict", local_file_path.extension().unwrap_or_default().to_str().unwrap_or(""))
                    );
                    fs::rename(&local_file_path, &conflict_path)?;
                    copy_file(&usb_sync_path, local_path, &path)?;
                    format!(
                        "[{}] 冲突: {} 已备份至 {}",
                        timestamp,
                        path.display(),
                        conflict_path.display()
                    )
                }
            };
            tx.send(SyncMessage::Log(message.clone()))?;
            log.push(message);
        }

        let final_sync_data = scan_directory(local_path)?;
        save_sync_data(&final_sync_data, &metadata_path)?;
        write_log_to_file(&log, &usb_sync_path)?;

        if log.is_empty() {
            tx.send(SyncMessage::Log("同步完成! 未检测到变化.".to_owned()))?;
        }
        Ok(())
    })() {
        Ok(_) => {},
        Err(e) => tx.send(SyncMessage::Log(format!("错误: {}", e))).unwrap_or_default(),
    }
    tx.send(SyncMessage::Complete).unwrap_or_default();
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

fn scan_directory(base_path: &Path) -> Result<SyncData, Box<dyn std::error::Error>> {
    let mut files = HashMap::new();
    for entry in WalkDir::new(base_path).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let path = entry.path().to_path_buf();
            if path.file_name().unwrap_or_default() == ".syncu_metadata.json"
                || path.file_name().unwrap_or_default() == "SyncU.log"
            {
                continue;
            }
            let metadata = fs::metadata(&path)?;
            let modified = metadata.modified()?;
            let content = fs::read(&path)?;
            let hash = format!("{:x}", Sha256::digest(&content));

            let relative_path = path.strip_prefix(base_path)?.to_path_buf();

            files.insert(
                relative_path.clone(),
                FileInfo {
                    path: relative_path,
                    hash,
                    modified,
                },
            );
        }
    }
    Ok(SyncData { files })
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

fn copy_file(from_dir: &Path, to_dir: &Path, relative_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let from = from_dir.join(relative_path);
    let to = to_dir.join(relative_path);

    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::copy(&from, &to)?;
    Ok(())
}

fn write_log_to_file(log: &[String], usb_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let log_path = usb_path.join("SyncU.log");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;

    for entry in log {
        writeln!(file, "{}", entry)?;
    }

    Ok(())
}
