use crate::models::{Resolution, SyncMessage};
use crate::sync::run_sync;
use crate::utils::find_usb_drives;
use eframe::egui;
use egui::{Color32, RichText};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

// Represents the state of a file conflict.
struct ConflictState {
    path: PathBuf,
}

// The main application structure.
pub struct SyncApp {
    local_folder: Option<PathBuf>,
    usb_drives: Vec<PathBuf>,
    selected_usb_drive: Option<PathBuf>,
    sync_log: Vec<RichText>,
    syncing: bool,
    stopping: bool,
    show_confirmation: bool,
    show_about_window: bool,
    show_conflict_resolution: bool,
    file_to_delete: Option<PathBuf>,
    conflict_state: Option<ConflictState>,
    deletion_choice: Option<bool>, // None: Ask, Some(true): Delete all, Some(false): Keep all
    progress: f32,
    current_file: String,
    tx_to_sync: Sender<SyncMessage>,
    rx_from_sync: Receiver<SyncMessage>,
    ctx: egui::Context,
}

impl SyncApp {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx_to_sync, rx_from_ui) = channel();
        let (tx_from_sync, rx_from_sync) = channel();

        // Spawn the synchronization thread
        let sync_thread_tx = tx_from_sync.clone();
        thread::spawn(move || {
            while let Ok(msg) = rx_from_ui.recv() {
                if let SyncMessage::StartSync(local, remote) = msg {
                    run_sync(Some(local), Some(remote), sync_thread_tx.clone(), &rx_from_ui);
                }
            }
        });

        let usb_drives = find_usb_drives();
        let selected_usb_drive = if usb_drives.len() == 1 {
            Some(usb_drives[0].clone())
        } else {
            None
        };

        Self {
            local_folder: None,
            usb_drives,
            selected_usb_drive,
            sync_log: vec![RichText::new("准备就绪").color(Color32::WHITE)],
            syncing: false,
            stopping: false,
            show_confirmation: false,
            show_about_window: false,
            show_conflict_resolution: false,
            file_to_delete: None,
            conflict_state: None,
            deletion_choice: None,
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
        // Process messages from the sync thread
        if let Ok(msg) = self.rx_from_sync.try_recv() {
            match msg {
                SyncMessage::Log(log) => {
                    let color = if log.starts_with("错误") {
                        Color32::from_rgb(210, 90, 90)
                    } else if log.starts_with("[") {
                        Color32::from_rgb(100, 180, 100)
                    } else {
                        ctx.style().visuals.text_color()
                    };
                    self.sync_log.push(RichText::new(log).color(color));
                }
                SyncMessage::ConfirmDeletion(path) => {
                    if let Some(choice) = self.deletion_choice {
                        self.tx_to_sync.send(SyncMessage::DeletionConfirmed(choice)).ok();
                    } else {
                        self.show_confirmation = true;
                        self.file_to_delete = Some(path);
                    }
                }
                SyncMessage::AskForConflictResolution { path } => {
                    self.show_conflict_resolution = true;
                    self.conflict_state = Some(ConflictState { path });
                }
                SyncMessage::Progress(progress, file) => {
                    self.progress = progress;
                    self.current_file = file;
                }
                SyncMessage::Complete => {
                    self.syncing = false;
                    self.stopping = false;
                    self.sync_log.push(RichText::new("同步完成!").color(Color32::from_rgb(0, 100, 0)));
                }
                SyncMessage::Stopped => {
                    self.syncing = false;
                    self.stopping = false;
                    self.sync_log.push(RichText::new("同步已停止.").color(Color32::from_rgb(210, 210, 90)));
                }
                _ => {}
            }
            self.ctx.request_repaint();
        }

        // --- UI Rendering ---
        if self.show_confirmation {
            egui::Window::new("确认删除").collapsible(false).resizable(false).show(ctx, |ui| {
                if let Some(file) = &self.file_to_delete {
                    ui.label(format!("您确定要删除文件\n'{}'?", file.display()));
                }
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        if ui.button("确认").clicked() {
                            self.tx_to_sync.send(SyncMessage::DeletionConfirmed(true)).ok();
                            self.show_confirmation = false;
                        }
                        if ui.button("取消").clicked() {
                            self.tx_to_sync.send(SyncMessage::DeletionConfirmed(false)).ok();
                            self.show_confirmation = false;
                        }
                    });
                    ui.add_space(5.0);
                    ui.horizontal(|ui| {
                        if ui.button("全部删除").clicked() {
                            self.deletion_choice = Some(true);
                            self.tx_to_sync.send(SyncMessage::DeletionConfirmed(true)).ok();
                            self.show_confirmation = false;
                        }
                        if ui.button("全部保留").clicked() {
                            self.deletion_choice = Some(false);
                            self.tx_to_sync.send(SyncMessage::DeletionConfirmed(false)).ok();
                            self.show_confirmation = false;
                        }
                    });
                });
            });
        }

        if self.show_conflict_resolution {
            if let Some(conflict) = &self.conflict_state {
                egui::Window::new(format!("解决冲突: {}", conflict.path.display()))
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.label("文件在本地和U盘上均被修改。请选择要保留的版本。");
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("采用本地版本").clicked() {
                                self.tx_to_sync.send(SyncMessage::ConflictResolved(Resolution::KeepLocal)).ok();
                                self.show_conflict_resolution = false;
                            }
                            if ui.button("采用U盘版本").clicked() {
                                self.tx_to_sync.send(SyncMessage::ConflictResolved(Resolution::KeepRemote)).ok();
                                self.show_conflict_resolution = false;
                            }
                            if ui.button("跳过").clicked() {
                                self.tx_to_sync.send(SyncMessage::ConflictResolved(Resolution::Skip)).ok();
                                self.show_conflict_resolution = false;
                            }
                        });
                    });
            }
        }

        if self.show_about_window {
            egui::Window::new("关于 SyncU").collapsible(false).resizable(false).show(ctx, |ui| {
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

        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("文件", |ui| {
                    if ui.button("关于").clicked() {
                        self.show_about_window = true;
                        ui.close_menu();
                    }
                    if ui.button("退出").clicked() {
                        _frame.close();
                    }
                });
            });
        });

        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            if self.syncing {
                ui.horizontal(|ui| {
                    ui.add(egui::ProgressBar::new(self.progress).show_percentage().desired_width(200.0));
                    ui.label(&self.current_file);
                });
            }
            else {
                ui.label(self.sync_log.last().cloned().unwrap_or_else(|| RichText::new("准备就绪")));
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            // When a dialog is shown, disable the main UI
            let main_ui_enabled = !self.show_conflict_resolution && !self.show_confirmation && !self.show_about_window;
            ui.add_enabled_ui(main_ui_enabled, |ui| {
                ui.heading("SyncU: Sync Your Files with USB");
                ui.add_space(10.0);

                ui.add_enabled_ui(!self.syncing, |ui| {
                    egui::Grid::new("path_grid").num_columns(3).spacing([10.0, 10.0]).show(ui, |ui| {
                        ui.label("本地文件夹:");
                        let local_path_text = self.local_folder.as_ref().map_or("未选择", |p| p.to_str().unwrap_or(""));
                        ui.label(RichText::new(local_path_text).strong());
                        if ui.button("选择...").clicked() {
                            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                self.local_folder = Some(path);
                            }
                        }
                        ui.end_row();

                        ui.label("U盘:");
                        if self.usb_drives.len() > 1 {
                            egui::ComboBox::from_label("")
                                .selected_text(self.selected_usb_drive.as_ref().map_or("请选择U盘", |p| p.to_str().unwrap_or("")))
                                .show_ui(ui, |ui| {
                                    for drive in &self.usb_drives {
                                        ui.selectable_value(&mut self.selected_usb_drive, Some(drive.clone()), drive.to_str().unwrap_or(""));
                                    }
                                });
                        } else {
                            let usb_path_text = self.selected_usb_drive.as_ref().map_or("未检测到", |p| p.to_str().unwrap_or(""));
                            ui.label(RichText::new(usb_path_text).strong());
                        }
                        if ui.button("刷新").clicked() {
                            self.usb_drives = find_usb_drives();
                            if self.usb_drives.len() == 1 {
                                self.selected_usb_drive = Some(self.usb_drives[0].clone());
                            }
                        }
                        ui.end_row();
                    });
                });

                ui.separator();
                ui.add_space(10.0);

                ui.vertical_centered_justified(|ui| {
                    if self.syncing {
                        let button_text = if self.stopping { "正在停止..." } else { "停止同步" };
                        let button_color = if self.stopping { Color32::DARK_GRAY } else { Color32::from_rgb(200, 0, 0) };
                        let stop_button = egui::Button::new(RichText::new(button_text).color(Color32::WHITE)).fill(button_color).min_size(egui::vec2(120.0, 40.0));
                        if ui.add_enabled(!self.stopping, stop_button).clicked() {
                            self.stopping = true;
                            self.tx_to_sync.send(SyncMessage::Stop).ok();
                        }
                    } else {
                        let enabled = self.local_folder.is_some() && self.selected_usb_drive.is_some();
                        let sync_button = egui::Button::new("立即同步").min_size(egui::vec2(120.0, 40.0));
                        if ui.add_enabled(enabled, sync_button).clicked() {
                            self.syncing = true;
                            self.deletion_choice = None; // Reset deletion choice at the start of a new sync
                            self.sync_log = vec![RichText::new("正在开始同步...").color(Color32::WHITE)];
                            if let (Some(local), Some(usb)) = (self.local_folder.clone(), self.selected_usb_drive.clone()) {
                                self.tx_to_sync.send(SyncMessage::StartSync(local, usb)).ok();
                            }
                        }
                    }
                });

                ui.add_space(10.0);
                ui.separator();

                egui::Frame::canvas(ui.style()).show(ui, |ui| {
                    egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false; 2]).show(ui, |ui| {
                        for log in &self.sync_log {
                            ui.label(log.clone());
                        }
                    });
                });
            });
        });
    }
}