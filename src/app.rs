use crate::models::{Resolution, SyncMessage, Theme};
use crate::sync::run_sync;
use crate::utils::find_usb_drives;
use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui;
use egui::{Color32, RichText};
use std::path::PathBuf;
use std::thread::{self, JoinHandle};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

// Represents the state of a file conflict.
struct ConflictState {
    path: PathBuf,
}

// Represents the application's current synchronization state.
#[derive(PartialEq)]
enum SyncState {
    Idle,
    Syncing,
    Stopping,
}

// The main application structure.
pub struct SyncApp {
    local_folder: Option<PathBuf>,
    usb_drives: Vec<PathBuf>,
    selected_usb_drive: Option<PathBuf>,
    sync_log: Vec<RichText>,
    state: SyncState,
    show_confirmation: bool,
    show_about_window: bool,
    show_conflict_resolution: bool,
    show_error_dialog: bool,
    error_message: String,
    file_to_delete: Option<PathBuf>,
    conflict_state: Option<ConflictState>,
    deletion_choice: Option<bool>, // None: Ask, Some(true): Delete all, Some(false): Keep all
    progress: f32,
    current_file: String,
    // We need a channel for each sync operation, so we create them on demand.
    tx_to_sync: Option<Sender<SyncMessage>>,
    rx_from_sync: Receiver<SyncMessage>,
    // The handle to the current sync thread.
    sync_thread: Option<JoinHandle<()>>,
    ctx: egui::Context,
    pub current_theme: Theme,
}

impl SyncApp {
    pub fn new(ctx: egui::Context) -> Self {
        // The main receiver for all sync threads.
        let (_, rx_from_sync) = unbounded();

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
            sync_log: vec![RichText::new("准备就绪").color(Color32::from_rgb(0, 100, 0))],
            state: SyncState::Idle,
            show_confirmation: false,
            show_about_window: false,
            show_conflict_resolution: false,
            show_error_dialog: false,
            error_message: "".to_string(),
            file_to_delete: None,
            conflict_state: None,
            deletion_choice: None,
            progress: 0.0,
            current_file: "".to_owned(),
            tx_to_sync: None,
            rx_from_sync,
            sync_thread: None,
            ctx,
            current_theme: Theme::Light,
        }
    }
}

impl eframe::App for SyncApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        crate::apply_theme(ctx, &self.current_theme);

        // Process all available messages from the sync thread in one go
        while let Ok(msg) = self.rx_from_sync.try_recv() {
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
                        if let Some(tx) = &self.tx_to_sync {
                            tx.send(SyncMessage::DeletionConfirmed(choice)).ok();
                        }
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
                    self.state = SyncState::Idle;
                    self.sync_log
                        .push(RichText::new("同步完成!").color(Color32::from_rgb(0, 100, 0)));
                }
                SyncMessage::Stopped => {
                    self.state = SyncState::Idle;
                    self.sync_log
                        .push(RichText::new("同步已停止.").color(Color32::from_rgb(210, 210, 90)));
                }
                _ => {}
            }
            self.ctx.request_repaint();
        }

        if self.show_error_dialog {
            egui::Window::new("错误")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.add_space(15.0);
                    ui.label(&self.error_message);
                    ui.add_space(10.0);
                    ui.separator();
                    ui.vertical_centered(|ui| {
                        if ui.button("关闭").clicked() {
                            self.show_error_dialog = false;
                        }
                    });
                });
        }

        if self.show_confirmation {
            egui::Window::new("确认删除")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.add_space(15.0);
                    if let Some(path) = &self.file_to_delete {
                        let item_type = if path.is_dir() { "目录" } else { "文件" };
                        ui.label(format!("您确定要删除{item_type}\n'{}'？", path.display()));
                    }
                    ui.add_space(10.0);
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            if ui.button("确认").clicked() {
                                if let Some(tx) = &self.tx_to_sync {
                                    tx.send(SyncMessage::DeletionConfirmed(true)).ok();
                                }
                                self.show_confirmation = false;
                            }
                            if ui.button("取消").clicked() {
                                if let Some(tx) = &self.tx_to_sync {
                                    tx.send(SyncMessage::DeletionConfirmed(false)).ok();
                                }
                                self.show_confirmation = false;
                            }
                            if ui.button("全部删除").clicked() {
                                self.deletion_choice = Some(true);
                                if let Some(tx) = &self.tx_to_sync {
                                    tx.send(SyncMessage::DeletionConfirmed(true)).ok();
                                }
                                self.show_confirmation = false;
                            }
                            if ui.button("全部保留").clicked() {
                                self.deletion_choice = Some(false);
                                if let Some(tx) = &self.tx_to_sync {
                                    tx.send(SyncMessage::DeletionConfirmed(false)).ok();
                                }
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
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.add_space(15.0);
                        ui.label("文件在本地和U盘上均被修改。请选择要保留的版本。");
                        ui.add_space(10.0);
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("采用本地版本").clicked() {
                                if let Some(tx) = &self.tx_to_sync {
                                    tx.send(SyncMessage::ConflictResolved(Resolution::KeepLocal))
                                        .ok();
                                }
                                self.show_conflict_resolution = false;
                            }
                            if ui.button("采用U盘版本").clicked() {
                                if let Some(tx) = &self.tx_to_sync {
                                    tx.send(SyncMessage::ConflictResolved(Resolution::KeepRemote))
                                        .ok();
                                }
                                self.show_conflict_resolution = false;
                            }
                            if ui.button("跳过").clicked() {
                                if let Some(tx) = &self.tx_to_sync {
                                    tx.send(SyncMessage::ConflictResolved(Resolution::Skip))
                                        .ok();
                                }
                                self.show_conflict_resolution = false;
                            }
                        });
                    });
            }
        }

        if self.show_about_window {
            egui::Window::new("关于 SyncU")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.add_space(15.0);
                    ui.vertical_centered(|ui| {
                        ui.heading("SyncU");
                        ui.add_space(10.0);
                        ui.label(format!("版本: {}", APP_VERSION));
                        ui.add_space(10.0);
                        ui.label("作者: Eight_Eggs");
                        ui.add_space(10.0);
                        ui.hyperlink_to("访问 GitHub 仓库", "https://github.com/EightEggs/SyncU");
                    });
                    ui.add_space(10.0);
                    ui.separator();
                    ui.vertical_centered(|ui| {
                        if ui.button("关闭").clicked() {
                            self.show_about_window = false;
                        }
                    });
                });
        }

        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.menu_button("文件", |ui| {
                    if ui.button("关于").clicked() {
                        self.show_about_window = true;
                        ui.close();
                    }
                    if ui.button("退出").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.separator();
                ui.menu_button("主题", |ui| {
                    if ui
                        .selectable_value(&mut self.current_theme, Theme::Light, "明亮")
                        .clicked()
                    {
                        ui.close();
                    }
                    if ui
                        .selectable_value(&mut self.current_theme, Theme::Dark, "暗黑")
                        .clicked()
                    {
                        ui.close();
                    }
                });
            });
        });

        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.add_space(4.0);
            if self.state != SyncState::Idle {
                ui.horizontal(|ui| {
                    if self.state == SyncState::Syncing {
                        ui.add(egui::Spinner::new());
                    }
                    ui.add(egui::ProgressBar::new(self.progress).desired_width(200.0));
                    ui.label(&self.current_file);
                });
            } else {
                ui.horizontal(|ui| {
                    ui.label(
                        self.sync_log
                            .last()
                            .cloned()
                            .unwrap_or_else(|| RichText::new("准备就绪")),
                    );
                });
            }
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            // When a dialog is shown, disable the main UI
            let main_ui_enabled = !self.show_conflict_resolution
                && !self.show_confirmation
                && !self.show_about_window
                && !self.show_error_dialog;
            ui.add_enabled_ui(main_ui_enabled, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(5.0);
                    ui.heading("SyncU: Sync Your Files with USB");
                    ui.add_space(5.0);
                });

                ui.add_space(1.0);
                ui.add_enabled_ui(self.state == SyncState::Idle, |ui| {
                    ui.vertical_centered(|ui| {
                        egui::Frame::group(ui.style())
                            .corner_radius(egui::CornerRadius::same(8))
                            .inner_margin(egui::Margin::same(12))
                            .show(ui, |ui| {
                                // Use vertical layout for rows
                                ui.vertical(|ui| {
                                    // First row: Local folder
                                    ui.horizontal(|ui| {
                                        ui.label("本地:");
                                        let local_path_text = self
                                            .local_folder
                                            .as_ref()
                                            .map_or("未选择", |p| p.to_str().unwrap_or(""));
                                        ui.label(RichText::new(local_path_text).weak());

                                        // Align button to the right
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            if ui.button("选择...").clicked() {
                                                if let Some(path) = rfd::FileDialog::new().pick_folder()
                                                {
                                                    let is_usb = self
                                                        .usb_drives
                                                        .iter()
                                                        .any(|usb| path.starts_with(usb));
                                                    if is_usb {
                                                        self.error_message =
                                                            "不能选择U盘或其子文件夹作为本地文件夹。"
                                                                .to_string();
                                                        self.show_error_dialog = true;
                                                    } else {
                                                        self.local_folder = Some(path);
                                                    }
                                                }
                                            }
                                        });
                                    });

                                    ui.add_space(5.0); // spacing between rows

                                    // Second row: USB drive
                                    ui.horizontal(|ui| {
                                        ui.label("U盘:");
                                        if self.usb_drives.len() > 1 {
                                            egui::ComboBox::from_label("")
                                                .selected_text(
                                                    self.selected_usb_drive
                                                        .as_ref()
                                                        .map_or("请选择U盘", |p| {
                                                            p.to_str().unwrap_or("")
                                                        }),
                                                )
                                                .show_ui(ui, |ui| {
                                                    for drive in &self.usb_drives {
                                                        ui.selectable_value(
                                                            &mut self.selected_usb_drive,
                                                            Some(drive.clone()),
                                                            drive.to_str().unwrap_or(""),
                                                        );
                                                    }
                                                });
                                        } else {
                                            let usb_path_text = self
                                                .selected_usb_drive
                                                .as_ref()
                                                .map_or("未检测到", |p| {
                                                    p.to_str().unwrap_or("")
                                                });
                                            ui.label(RichText::new(usb_path_text).weak());
                                        }

                                        // Align button to the right
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            if ui.button(" 刷新 ").clicked() {
                                                self.usb_drives = find_usb_drives();
                                                if self.usb_drives.len() == 1 {
                                                    self.selected_usb_drive =
                                                        Some(self.usb_drives[0].clone());
                                                }
                                            }
                                        });
                                    });
                                });
                            });
                    });
                });

                ui.add_space(5.0);

                ui.vertical_centered(|ui| {
                    match self.state {
                        SyncState::Idle => {
                            let enabled =
                                self.local_folder.is_some() && self.selected_usb_drive.is_some();
                            let sync_button = egui::Button::new(RichText::new("立即同步"))
                                .corner_radius(egui::CornerRadius::same(6))
                                .min_size(egui::vec2(250.0, 40.0));
                            if ui.add_enabled(enabled, sync_button).clicked() {
                                self.state = SyncState::Syncing;
                                self.deletion_choice = None; // Reset deletion choice
                                self.sync_log = vec![RichText::new("正在开始同步...")
                                    .color(Color32::from_rgb(0, 100, 0))];

                                if let (Some(local), Some(usb)) =
                                    (self.local_folder.clone(), self.selected_usb_drive.clone())
                                {
                                    // Create new channels for this specific sync task.
                                    let (tx_to_sync, rx_from_ui) = unbounded();
                                    let (tx_from_sync, rx_from_sync) = unbounded();
                                    self.tx_to_sync = Some(tx_to_sync);
                                    self.rx_from_sync = rx_from_sync;

                                    let sync_thread = thread::spawn(move || {
                                        run_sync(
                                            Some(local),
                                            Some(usb),
                                            tx_from_sync,
                                            rx_from_ui,
                                        );
                                    });
                                    self.sync_thread = Some(sync_thread);
                                }
                            }
                        }
                        SyncState::Syncing => {
                            let stop_button = egui::Button::new(
                                RichText::new("停止同步").color(egui::Color32::WHITE),
                            )
                            .corner_radius(egui::CornerRadius::same(6))
                            .min_size(egui::vec2(250.0, 40.0))
                            .fill(Color32::from_rgb(200, 30, 70));
                            if ui.add(stop_button).clicked() {
                                self.state = SyncState::Stopping;
                                if let Some(tx) = &self.tx_to_sync {
                                    tx.send(SyncMessage::Stop).ok();
                                }
                            }
                        }
                        SyncState::Stopping => {
                            let stop_button = egui::Button::new(
                                RichText::new("正在停止...").color(egui::Color32::WHITE),
                            )
                            .corner_radius(egui::CornerRadius::same(6))
                            .min_size(egui::vec2(250.0, 40.0))
                            .fill(Color32::from_rgb(200, 30, 70));
                            ui.add_enabled(false, stop_button);
                        }
                    }
                });

                ui.add_space(5.0);

                egui::Frame::group(ui.style())
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        ui.heading(RichText::new("日志").size(16.0));
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .max_height(234.0)
                            .stick_to_bottom(true)
                            .auto_shrink([false; 2])
                            .show(ui, |ui| {
                                for log in &self.sync_log {
                                    ui.label(log.clone());
                                }
                            });
                    });
            });
        });
    }
}
