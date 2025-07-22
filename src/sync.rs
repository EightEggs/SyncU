use crate::models::{SyncAction, SyncMessage};
use crate::utils::{
    copy_large_file_with_progress, load_sync_data, save_sync_data, scan_directory_with_progress,
    write_log_entry,
};
use chrono::Local;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use walkdir::WalkDir;

const LARGE_FILE_THRESHOLD: u64 = 10 * 1024 * 1024; // 10 MB

pub fn run_sync(
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

        tx.send(SyncMessage::Progress(
            0.0,
            "正在加载上次同步记录...".to_string(),
        ))?;
        let last_sync_data = load_sync_data(&metadata_path)?;

        // Scan local directory with progress
        tx.send(SyncMessage::Progress(
            0.0,
            "正在统计本地文件...".to_string(),
        ))?;
        let local_total = WalkDir::new(local_path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .count();
        let local_sync_data =
            match scan_directory_with_progress(local_path, &tx, rx, local_total, "扫描本地")? {
                Some(data) => data,
                None => return Ok(true), // Stopped
            };

        // Scan remote directory with progress
        tx.send(SyncMessage::Progress(0.0, "正在统计U盘文件...".to_string()))?;
        let remote_total = WalkDir::new(&usb_sync_path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .count();
        let remote_sync_data =
            match scan_directory_with_progress(&usb_sync_path, &tx, rx, remote_total, "扫描U盘")?
            {
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

        tx.send(SyncMessage::Progress(
            0.0,
            "正在分析文件差异...".to_string(),
        ))?;
        for relative_path in all_files.iter() {
            if let Ok(SyncMessage::Stop) = rx.try_recv() {
                return Ok(true);
            }

            let local_file = local_sync_data.files.get(relative_path);
            let remote_file = remote_sync_data.files.get(relative_path);
            let last_sync_file = last_sync_data.files.get(relative_path);

            match (local_file, remote_file, last_sync_file) {
                (Some(local), Some(remote), Some(last)) => {
                    let local_changed = local.modified > last.modified;
                    let remote_changed = remote.modified > last.modified;

                    if local_changed && remote_changed {
                        sync_plan.push(SyncAction::Conflict(relative_path.clone()));
                    } else if local_changed {
                        sync_plan.push(SyncAction::Upload(relative_path.clone()));
                    } else if remote_changed {
                        sync_plan.push(SyncAction::Download(relative_path.clone()));
                    }
                }
                (Some(_local), None, None) => {
                    sync_plan.push(SyncAction::Upload(relative_path.clone()))
                }
                (None, Some(_remote), None) => {
                    sync_plan.push(SyncAction::Download(relative_path.clone()))
                }
                (None, Some(_remote), Some(_last)) => {
                    sync_plan.push(SyncAction::DeleteRemote(relative_path.clone()))
                }
                (Some(_local), None, Some(_last)) => {
                    sync_plan.push(SyncAction::DeleteLocal(relative_path.clone()))
                }
                _ => {}
            }
        }

        let total_sync_size = sync_plan.iter().try_fold(
            0u64,
            |acc, action| -> Result<u64, Box<dyn std::error::Error>> {
                Ok(acc
                    + match action {
                        SyncAction::Upload(path) => fs::metadata(local_path.join(path))?.len(),
                        SyncAction::Download(path) => fs::metadata(usb_sync_path.join(path))?.len(),
                        SyncAction::Conflict(path) => fs::metadata(local_path.join(path))?.len(),
                        _ => 0,
                    })
            },
        )?;

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
                SyncAction::Upload(path)
                | SyncAction::Download(path)
                | SyncAction::Conflict(path) => path.to_str().unwrap_or_default().to_string(),
                SyncAction::DeleteLocal(path) | SyncAction::DeleteRemote(path) => {
                    format!("删除: {}", path.to_str().unwrap_or_default())
                }
            };

            let message = match action {
                SyncAction::Upload(path) => {
                    let from = local_path.join(&path);
                    let to = usb_sync_path.join(&path);
                    if let Some(parent) = to.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    if file_size > LARGE_FILE_THRESHOLD {
                        if copy_large_file_with_progress(
                            &from,
                            &to,
                            &current_file_name,
                            &tx,
                            rx,
                            total_sync_size,
                            processed_size,
                        )? {
                            return Ok(true); // Stopped
                        }
                    } else {
                        fs::copy(&from, &to)?;
                    }
                    format!(
                        "[{}] 上传: 本地 -> U盘: {}",
                        Local::now().format("%Y-%m-%d %H:%M:%S"),
                        path.display()
                    )
                }
                SyncAction::Download(path) => {
                    let from = usb_sync_path.join(&path);
                    let to = local_path.join(&path);
                    if let Some(parent) = to.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    if file_size > LARGE_FILE_THRESHOLD {
                        if copy_large_file_with_progress(
                            &from,
                            &to,
                            &current_file_name,
                            &tx,
                            rx,
                            total_sync_size,
                            processed_size,
                        )? {
                            return Ok(true); // Stopped
                        }
                    } else {
                        fs::copy(&from, &to)?;
                    }
                    format!(
                        "[{}] 下载: U盘 -> 本地: {}",
                        Local::now().format("%Y-%m-%d %H:%M:%S"),
                        path.display()
                    )
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
                        format!(
                            "[{}] 删除: U盘: {}",
                            Local::now().format("%Y-%m-%d %H:%M:%S"),
                            path.display()
                        )
                    } else {
                        format!(
                            "[{}] 取消删除: U盘: {}",
                            Local::now().format("%Y-%m-%d %H:%M:%S"),
                            path.display()
                        )
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
                        format!(
                            "[{}] 删除: 本地: {}",
                            Local::now().format("%Y-%m-%d %H:%M:%S"),
                            path.display()
                        )
                    } else {
                        format!(
                            "[{}] 取消删除: 本地: {}",
                            Local::now().format("%Y-%m-%d %H:%M:%S"),
                            path.display()
                        )
                    }
                }
                SyncAction::Conflict(path) => {
                    let local_file_path = local_path.join(&path);
                    let conflict_path = local_file_path.with_extension(format!(
                        "{}.conflict",
                        local_file_path
                            .extension()
                            .unwrap_or_default()
                            .to_str()
                            .unwrap_or("")
                    ));
                    fs::rename(&local_file_path, &conflict_path)?;
                    let from = usb_sync_path.join(&path);
                    let to = local_path.join(&path);
                    if let Some(parent) = to.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    if file_size > LARGE_FILE_THRESHOLD {
                        if copy_large_file_with_progress(
                            &from,
                            &to,
                            &current_file_name,
                            &tx,
                            rx,
                            total_sync_size,
                            processed_size,
                        )? {
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
                tx.send(SyncMessage::Progress(
                    processed_size as f32 / total_sync_size as f32,
                    current_file_name,
                ))?;
            }
            tx.send(SyncMessage::Log(message.clone()))?;
            write_log_entry(&message, &usb_sync_path)?;
        }

        let final_sync_data =
            scan_directory_with_progress(local_path, &tx, rx, local_total, "更新本地元数据")?;
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
        let msg = format!(
            "[{}] 同步已由用户停止。",
            Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        tx.send(SyncMessage::Log(msg)).unwrap_or_default();
        tx.send(SyncMessage::Stopped).unwrap_or_default();
    } else {
        tx.send(SyncMessage::Complete).unwrap_or_default();
    }
}
