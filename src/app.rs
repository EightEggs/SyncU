use crate::models::SyncMessage;
use crate::sync::run_sync;
use crate::utils::find_usb_drives;
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct SyncApp {
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
    pub fn new(ctx: egui::Context) -> Self {
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
        let selected_usb_drive = if usb_drives.len() == 1 {
            Some(usb_drives[0].clone())
        } else {
            None
        };

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
                            self.tx_to_sync
                                .send(SyncMessage::DeletionConfirmed(true))
                                .unwrap_or_default();
                            self.show_confirmation = false;
                        }
                        if ui.button("取消").clicked() {
                            self.tx_to_sync
                                .send(SyncMessage::DeletionConfirmed(false))
                                .unwrap_or_default();
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
                    ui.add(
                        egui::ProgressBar::new(self.progress)
                            .show_percentage()
                            .desired_width(200.0),
                    );
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
                        let local_path_text = self
                            .local_folder
                            .as_ref()
                            .map_or_else(|| "未选择".to_owned(), |p| p.display().to_string());
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
                                .selected_text(
                                    self.selected_usb_drive.as_ref().map_or("请选择U盘", |p| {
                                        p.to_str().unwrap_or_default()
                                    }),
                                )
                                .show_ui(ui, |ui| {
                                    for drive in &self.usb_drives {
                                        ui.selectable_value(
                                            &mut self.selected_usb_drive,
                                            Some(drive.clone()),
                                            drive.to_str().unwrap_or_default(),
                                        );
                                    }
                                });
                        } else {
                            let usb_path_text = self
                                .selected_usb_drive
                                .as_ref()
                                .map_or_else(|| "未检测到".to_owned(), |p| p.display().to_string());
                            ui.label(&usb_path_text);
                        }
                        if ui.button("刷新").clicked() {
                            self.usb_drives = find_usb_drives();
                            self.selected_usb_drive = if self.usb_drives.len() == 1 {
                                Some(self.usb_drives[0].clone())
                            } else {
                                None
                            };
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
                        let enabled =
                            self.local_folder.is_some() && self.selected_usb_drive.is_some();
                        if ui
                            .add_enabled(
                                enabled,
                                egui::Button::new("立即同步").min_size(egui::vec2(120.0, 40.0)),
                            )
                            .clicked()
                        {
                            self.syncing = true;
                            self.sync_log = vec!["同步中...".to_owned()];
                            if let (Some(local_folder), Some(selected_usb_drive)) =
                                (self.local_folder.as_ref(), self.selected_usb_drive.as_ref())
                            {
                                self.tx_to_sync
                                    .send(SyncMessage::Log(
                                        local_folder.to_str().unwrap_or_default().to_string(),
                                    ))
                                    .unwrap_or_default();
                                self.tx_to_sync
                                    .send(SyncMessage::Log(
                                        selected_usb_drive.to_str().unwrap_or_default().to_string(),
                                    ))
                                    .unwrap_or_default();
                            }
                        }
                    }
                });
            });

            ui.add_space(10.0);
            ui.separator();

            egui::Frame::canvas(ui.style()).show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(self.sync_log.join("\n"));
                });
            });
        });
    }
}
