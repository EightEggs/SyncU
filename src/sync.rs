use crate::models::{Resolution, SyncAction, SyncData, SyncMessage};
use crate::utils::{copy_large_file_with_progress, load_sync_data, save_sync_data, scan_directory_with_progress, write_log_entry};
use chrono::Local;
use crossbeam_channel::{Receiver, RecvTimeoutError};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use walkdir::WalkDir;

const LARGE_FILE_THRESHOLD: u64 = 10 * 1024 * 1024; // 10 MB

/// Helper function to wait for a specific message while also checking for a stop signal.
fn wait_for_message<F, T>(rx: &Receiver<SyncMessage>, mut condition: F) -> Result<Option<T>, ()>
where
    F: FnMut(SyncMessage) -> Option<T>,
{
    loop {
        // Use a timeout to prevent blocking indefinitely.
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(msg) => {
                if let SyncMessage::Stop = msg {
                    return Err(()); // Stop signal received
                }
                if let Some(result) = condition(msg) {
                    return Ok(Some(result)); // Desired message received
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                // Timeout is not an error, just continue the loop to check for stop signal again.
            }
            Err(RecvTimeoutError::Disconnected) => {
                // Channel disconnected, treat as a stop.
                return Err(());
            }
        }
    }
}

pub fn run_sync(
    local_folder: Option<PathBuf>,
    usb_drive: Option<PathBuf>,
    tx: crossbeam_channel::Sender<SyncMessage>,
    rx: Receiver<SyncMessage>,
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

        if rx.try_recv() == Ok(SyncMessage::Stop) { return Ok(true); }
        tx.send(SyncMessage::Progress(0.0, "正在统计本地文件...".to_string()))?;
        let local_total = WalkDir::new(local_path).into_iter().filter_map(Result::ok).filter(|e| e.file_type().is_file()).count();
        let local_sync_data =
            match scan_directory_with_progress(local_path, &tx, &rx, local_total, "扫描本地", &last_sync_data)? {
                Some(data) => data,
                None => return Ok(true), // Stopped
            };

        if rx.try_recv() == Ok(SyncMessage::Stop) { return Ok(true); }
        tx.send(SyncMessage::Progress(0.0, "正在统计U盘文件...".to_string()))?;
        let remote_total = WalkDir::new(&usb_sync_path).into_iter().filter_map(Result::ok).filter(|e| e.file_type().is_file()).count();
        let remote_sync_data =
            match scan_directory_with_progress(&usb_sync_path, &tx, &rx, remote_total, "扫描U盘", &last_sync_data)?
            {
                Some(data) => data,
                None => return Ok(true), // Stopped
            };

        tx.send(SyncMessage::Progress(0.0, "正在分析文件差异...".to_string()))?;

        let mut all_paths = HashSet::new();
        all_paths.extend(last_sync_data.files.keys().cloned());
        all_paths.extend(local_sync_data.files.keys().cloned());
        all_paths.extend(remote_sync_data.files.keys().cloned());

        let mut sync_plan = Vec::new();

        for path in all_paths {
            if rx.try_recv() == Ok(SyncMessage::Stop) {
                return Ok(true);
            }

            let last_info = last_sync_data.files.get(&path);
            let local_info = local_sync_data.files.get(&path);
            let remote_info = remote_sync_data.files.get(&path);

            let action = match (local_info, remote_info, last_info) {
                (Some(local), Some(remote), Some(last)) => {
                    let local_changed = local.hash != last.hash;
                    let remote_changed = remote.hash != last.hash;
                    if local_changed && remote_changed { Some(SyncAction::Conflict { path: path.clone() }) }
                    else if local_changed { Some(SyncAction::LocalToRemote(path.clone())) }
                    else if remote_changed { Some(SyncAction::RemoteToLocal(path.clone())) }
                    else { None }
                }
                (Some(local), Some(remote), None) => {
                    if local.hash == remote.hash { None }
                    else { Some(SyncAction::Conflict { path: path.clone() }) }
                }
                (Some(_), None, Some(_)) => Some(SyncAction::DeleteLocal(path.clone())),
                (None, Some(_), Some(_)) => Some(SyncAction::DeleteRemote(path.clone())),
                (Some(_), None, None) => Some(SyncAction::LocalToRemote(path.clone())),
                (None, Some(_), None) => Some(SyncAction::RemoteToLocal(path.clone())),
                _ => None,
            };

            if let Some(action) = action {
                sync_plan.push(action);
            }
        }

        let total_sync_size = sync_plan.iter().try_fold(0u64, |acc, action| -> Result<u64, Box<dyn std::error::Error>> {
            Ok(acc + match action {
                SyncAction::LocalToRemote(path) => fs::metadata(local_path.join(path))?.len(),
                SyncAction::RemoteToLocal(path) => fs::metadata(usb_sync_path.join(path))?.len(),
                SyncAction::Conflict { path, .. } => fs::metadata(local_path.join(path))?.len(),
                _ => 0,
            })
        })?;

        let mut skipped_files = HashSet::new();
        let mut processed_size = 0u64;
        let sync_plan_len = sync_plan.len();

        if sync_plan.is_empty() {
            tx.send(SyncMessage::Log("未检测到变化.".to_owned()))?;
        } else {
            tx.send(SyncMessage::Log(format!("计划执行 {} 个同步操作...", sync_plan_len)))?;
        }

        for (index, action) in sync_plan.iter().enumerate() {
            if rx.try_recv() == Ok(SyncMessage::Stop) {
                return Ok(true);
            }

            let (file_size, current_file_name) = match action {
                SyncAction::LocalToRemote(path) | SyncAction::RemoteToLocal(path) | SyncAction::Conflict { path, .. } => {
                    let full_path = if matches!(action, SyncAction::RemoteToLocal(_)) { usb_sync_path.join(path) } else { local_path.join(path) };
                    (fs::metadata(&full_path).map(|m| m.len()).unwrap_or(0), path.to_str().unwrap_or("").to_string())
                }
                SyncAction::DeleteLocal(path) | SyncAction::DeleteRemote(path) => {
                    (0, format!("删除: {}", path.to_str().unwrap_or("")))
                }
            };

            let progress = if total_sync_size > 0 { processed_size as f32 / total_sync_size as f32 } else { 0.0 };
            tx.send(SyncMessage::Progress(progress, format!("({}/{})正在处理: {}", index + 1, sync_plan_len, current_file_name)))?;

            let message = match action {
                SyncAction::LocalToRemote(path) => {
                    let from = local_path.join(path);
                    let to = usb_sync_path.join(path);
                    if let Some(parent) = to.parent() { fs::create_dir_all(parent)?; }
                    if fs::metadata(&from)?.len() > LARGE_FILE_THRESHOLD {
                        if copy_large_file_with_progress(&from, &to, &current_file_name, &tx, &rx, total_sync_size, processed_size)? {
                            return Ok(true); // Stopped
                        }
                    } else { fs::copy(&from, &to)?; }
                    format!("[{}] 本地 -> U盘: {}", Local::now().format("%H:%M:%S"), from.strip_prefix(local_path)?.display())
                }
                SyncAction::RemoteToLocal(path) => {
                    let from = usb_sync_path.join(path);
                    let to = local_path.join(path);
                    if let Some(parent) = to.parent() { fs::create_dir_all(parent)?; }
                    if fs::metadata(&from)?.len() > LARGE_FILE_THRESHOLD {
                        if copy_large_file_with_progress(&from, &to, &current_file_name, &tx, &rx, total_sync_size, processed_size)? {
                            return Ok(true); // Stopped
                        }
                    } else { fs::copy(&from, &to)?; }
                    format!("[{}] U盘 -> 本地: {}", Local::now().format("%H:%M:%S"), from.strip_prefix(&usb_sync_path)?.display())
                }
                SyncAction::DeleteRemote(path) => {
                    let absolute_path = usb_sync_path.join(path);
                    tx.send(SyncMessage::ConfirmDeletion(absolute_path.clone()))?;
                    let confirmed = match wait_for_message(&rx, |msg| match msg {
                        SyncMessage::DeletionConfirmed(c) => Some(c),
                        _ => None,
                    }) {
                        Ok(Some(c)) => c,
                        _ => return Ok(true), // Stopped or disconnected
                    };
                    if confirmed {
                        if absolute_path.exists() { fs::remove_file(absolute_path)?; }
                        format!("[{}] 删除U盘文件: {}", Local::now().format("%H:%M:%S"), path.display())
                    } else {
                        format!("[{}] 取消删除: {}", Local::now().format("%H:%M:%S"), path.display())
                    }
                }
                SyncAction::DeleteLocal(path) => {
                    let absolute_path = local_path.join(path);
                    tx.send(SyncMessage::ConfirmDeletion(absolute_path.clone()))?;
                    let confirmed = match wait_for_message(&rx, |msg| match msg {
                        SyncMessage::DeletionConfirmed(c) => Some(c),
                        _ => None,
                    }) {
                        Ok(Some(c)) => c,
                        _ => return Ok(true), // Stopped or disconnected
                    };
                    if confirmed {
                        if absolute_path.exists() { fs::remove_file(absolute_path)?; }
                        format!("[{}] 删除本地文件: {}", Local::now().format("%H:%M:%S"), path.display())
                    } else {
                        format!("[{}] 取消删除: {}", Local::now().format("%H:%M:%S"), path.display())
                    }
                }
                SyncAction::Conflict { path } => {
                    tx.send(SyncMessage::AskForConflictResolution { path: path.clone() })?;
                    let resolution = match wait_for_message(&rx, |msg| match msg {
                        SyncMessage::ConflictResolved(r) => Some(r),
                        _ => None,
                    }) {
                        Ok(Some(r)) => r,
                        _ => return Ok(true), // Stopped or disconnected
                    };

                    match resolution {
                        Resolution::KeepLocal => {
                            let from = local_path.join(path);
                            let to = usb_sync_path.join(path);
                            if let Some(parent) = to.parent() { fs::create_dir_all(parent)?; }
                            if fs::metadata(&from)?.len() > LARGE_FILE_THRESHOLD {
                                if copy_large_file_with_progress(&from, &to, &current_file_name, &tx, &rx, total_sync_size, processed_size)? {
                                    return Ok(true); // Stopped
                                }
                            } else { fs::copy(&from, &to)?; }
                            format!("[{}] 冲突解决 (采用本地): {}", Local::now().format("%H:%M:%S"), from.strip_prefix(local_path)?.display())
                        }
                        Resolution::KeepRemote => {
                            let from = usb_sync_path.join(path);
                            let to = local_path.join(path);
                            if let Some(parent) = to.parent() { fs::create_dir_all(parent)?; }
                            if fs::metadata(&from)?.len() > LARGE_FILE_THRESHOLD {
                                if copy_large_file_with_progress(&from, &to, &current_file_name, &tx, &rx, total_sync_size, processed_size)? {
                                    return Ok(true); // Stopped
                                }
                            } else { fs::copy(&from, &to)?; }
                            format!("[{}] 冲突解决 (采用U盘): {}", Local::now().format("%H:%M:%S"), from.strip_prefix(&usb_sync_path)?.display())
                        }
                        Resolution::Skip => {
                            skipped_files.insert(path.clone());
                            format!("[{}] 跳过冲突文件: {}", Local::now().format("%H:%M:%S"), path.display())
                        }
                    }
                }
            };
            processed_size += file_size;
            tx.send(SyncMessage::Log(message.clone()))?;
            write_log_entry(&message, &usb_sync_path)?;
        }

        if rx.try_recv() == Ok(SyncMessage::Stop) { return Ok(true); }
        tx.send(SyncMessage::Progress(0.99, "正在生成新的同步记录...".to_string()))?;
        let final_scan_result =
            scan_directory_with_progress(local_path, &tx, &rx, local_total, "更新本地元数据", &SyncData::default())?;

        if let Some(mut final_sync_data) = final_scan_result {
            final_sync_data.files.retain(|path, _| !skipped_files.contains(path));
            save_sync_data(&final_sync_data, &metadata_path)?;
        } else {
            return Ok(true); // Stopped during final scan
        }

        tx.send(SyncMessage::Progress(1.0, "同步完成!".to_string()))?;
        Ok(false)
    })() {
        Ok(stopped) => stopped,
        Err(e) => {
            let msg = format!("错误: {}", e);
            let _ = tx.send(SyncMessage::Log(msg.clone()));
            if let (Some(local_folder), Some(usb_drive)) = (local_folder, usb_drive) {
                let sync_folder_name = local_folder.file_name().unwrap();
                let usb_sync_path = usb_drive.join(sync_folder_name);
                let _ = write_log_entry(&msg, &usb_sync_path);
            }
            false
        }
    };

    if was_stopped {
        let msg = format!("[{}] 同步已由用户停止。", Local::now().format("%H:%M:%S"));
        let _ = tx.send(SyncMessage::Log(msg));
        let _ = tx.send(SyncMessage::Stopped);
    } else {
        let _ = tx.send(SyncMessage::Complete);
    }
}
