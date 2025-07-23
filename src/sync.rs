use crate::models::{Resolution, SyncAction, SyncData, SyncMessage};
use crate::utils::{copy_large_file_with_progress, load_sync_data, save_sync_data, scan_directory_with_progress, write_log_entry};
use chrono::Local;
use std::collections::HashSet;
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

        // --- PERFORMANCE OPTIMIZATION: Pass last_sync_data to avoid re-hashing everything ---
        tx.send(SyncMessage::Progress(0.0, "正在统计本地文件...".to_string()))?;
        let local_total = WalkDir::new(local_path).into_iter().filter_map(Result::ok).filter(|e| e.file_type().is_file()).count();
        let local_sync_data =
            match scan_directory_with_progress(local_path, &tx, rx, local_total, "扫描本地", &last_sync_data)? {
                Some(data) => data,
                None => return Ok(true), // Stopped
            };

        tx.send(SyncMessage::Progress(0.0, "正在统计U盘文件...".to_string()))?;
        let remote_total = WalkDir::new(&usb_sync_path).into_iter().filter_map(Result::ok).filter(|e| e.file_type().is_file()).count();
        let remote_sync_data =
            match scan_directory_with_progress(&usb_sync_path, &tx, rx, remote_total, "扫描U盘", &last_sync_data)?
            {
                Some(data) => data,
                None => return Ok(true), // Stopped
            };

        let mut sync_plan = Vec::new();

        // Combine all unique file paths from local, remote, and last sync states
        let all_files: HashSet<_> = local_sync_data.files.keys()
            .chain(remote_sync_data.files.keys())
            .chain(last_sync_data.files.keys())
            .cloned()
            .collect();

        tx.send(SyncMessage::Progress(0.0, "正在分析文件差异...".to_string()))?;
        for relative_path in all_files.iter() {
            if rx.try_recv() == Ok(SyncMessage::Stop) {
                return Ok(true);
            }

            let local_file = local_sync_data.files.get(relative_path);
            let remote_file = remote_sync_data.files.get(relative_path);
            let last_sync_file = last_sync_data.files.get(relative_path);

            // --- LOGIC CHANGE: Compare hashes instead of modification times for accuracy ---
            match (local_file, remote_file, last_sync_file) {
                // Case 1: File exists in all three states
                (Some(local), Some(remote), Some(last)) => {
                    let local_changed = local.hash != last.hash;
                    let remote_changed = remote.hash != last.hash;

                    if local.hash != remote.hash { // Hashes differ, potential conflict or update
                        if local_changed && remote_changed {
                            sync_plan.push(SyncAction::Conflict { path: relative_path.clone() });
                        } else if local_changed {
                            sync_plan.push(SyncAction::LocalToRemote(relative_path.clone()));
                        } else if remote_changed {
                            sync_plan.push(SyncAction::RemoteToLocal(relative_path.clone()));
                        }
                        // If hashes are same but different from last, one side was updated and synced. No action needed.
                    }
                }
                // Case 2: File added locally and remotely simultaneously (or one is a copy of the other)
                (Some(local), Some(remote), None) => {
                    if local.hash != remote.hash {
                        
                        sync_plan.push(SyncAction::Conflict { path: relative_path.clone() });
                    } // If hashes are identical, they are the same file. No action needed.
                }
                // Case 3: File exists locally and in last sync, but not on remote -> Deleted on remote
                (Some(_local), None, Some(_last)) => {
                    sync_plan.push(SyncAction::DeleteLocal(relative_path.clone()));
                }
                // Case 4: File exists on remote and in last sync, but not locally -> Deleted locally
                (None, Some(_remote), Some(_last)) => {
                    sync_plan.push(SyncAction::DeleteRemote(relative_path.clone()));
                }
                // Case 5: New file only on local -> Upload
                (Some(_local), None, None) => {
                    sync_plan.push(SyncAction::LocalToRemote(relative_path.clone()));
                }
                // Case 6: New file only on remote -> Download
                (None, Some(_remote), None) => {
                    sync_plan.push(SyncAction::RemoteToLocal(relative_path.clone()));
                }
                // Other cases (e.g., only in last_sync) are handled by cases 3 & 4.
                _ => {}
            }
        }

        let total_sync_size = sync_plan.iter().try_fold(0u64, |acc, action| -> Result<u64, Box<dyn std::error::Error>> {
            Ok(acc + match action {
                SyncAction::LocalToRemote(path) => fs::metadata(local_path.join(path))?.len(),
                SyncAction::RemoteToLocal(path) => fs::metadata(usb_sync_path.join(path))?.len(),
                SyncAction::Conflict { path, .. } => fs::metadata(local_path.join(path))?.len(), // Assume local size for conflict
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

        for (index, action) in sync_plan.into_iter().enumerate() {
            if rx.try_recv() == Ok(SyncMessage::Stop) {
                return Ok(true);
            }

            let (file_size, current_file_name) = match &action {
                SyncAction::LocalToRemote(path) | SyncAction::RemoteToLocal(path) | SyncAction::Conflict { path, .. } => {
                    let full_path = if matches!(action, SyncAction::RemoteToLocal(_)) { usb_sync_path.join(path) } else { local_path.join(path) };
                    (fs::metadata(full_path).map(|m| m.len()).unwrap_or(0), path.to_str().unwrap_or("").to_string())
                }
                SyncAction::DeleteLocal(path) | SyncAction::DeleteRemote(path) => {
                    (0, format!("删除: {}", path.to_str().unwrap_or("")))
                }
            };

            let progress = if total_sync_size > 0 { processed_size as f32 / total_sync_size as f32 } else { 0.0 };
            tx.send(SyncMessage::Progress(progress, format!("({}/{})正在处理: {}", index + 1, sync_plan_len, current_file_name)))?;

            let message = match action {
                SyncAction::LocalToRemote(path) => {
                    let from = local_path.join(&path);
                    let to = usb_sync_path.join(&path);
                    if let Some(parent) = to.parent() { fs::create_dir_all(parent)?; }
                    if from.metadata()?.len() > LARGE_FILE_THRESHOLD {
                        if copy_large_file_with_progress(&from, &to, &current_file_name, &tx, rx, total_sync_size, processed_size)? {
                            return Ok(true); // Stopped
                        }
                    } else {
                        fs::copy(&from, &to)?;
                    }
                    format!("[{}] 本地 -> U盘: {}", Local::now().format("%H:%M:%S"), path.display())
                }
                SyncAction::RemoteToLocal(path) => {
                    let from = usb_sync_path.join(&path);
                    let to = local_path.join(&path);
                    if let Some(parent) = to.parent() { fs::create_dir_all(parent)?; }
                    if from.metadata()?.len() > LARGE_FILE_THRESHOLD {
                        if copy_large_file_with_progress(&from, &to, &current_file_name, &tx, rx, total_sync_size, processed_size)? {
                            return Ok(true); // Stopped
                        }
                    } else {
                        fs::copy(&from, &to)?;
                    }
                    format!("[{}] U盘 -> 本地: {}", Local::now().format("%H:%M:%S"), path.display())
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
                        if absolute_path.exists() { fs::remove_file(absolute_path)?; }
                        format!("[{}] 删除U盘文件: {}", Local::now().format("%H:%M:%S"), path.display())
                    } else {
                        format!("[{}] 取消删除: {}", Local::now().format("%H:%M:%S"), path.display())
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
                        if absolute_path.exists() { fs::remove_file(absolute_path)?; }
                        format!("[{}] 删除本地文件: {}", Local::now().format("%H:%M:%S"), path.display())
                    } else {
                        format!("[{}] 取消删除: {}", Local::now().format("%H:%M:%S"), path.display())
                    }
                }
                SyncAction::Conflict { path } => {
                    tx.send(SyncMessage::AskForConflictResolution { path: path.clone() })?;
                    let resolution = loop {
                        match rx.recv()? {
                            SyncMessage::ConflictResolved(r) => break r,
                            SyncMessage::Stop => return Ok(true),
                            _ => {}
                        }
                    };

                    match resolution {
                        Resolution::KeepLocal => {
                            let from = local_path.join(&path);
                            let to = usb_sync_path.join(&path);
                            if let Some(parent) = to.parent() { fs::create_dir_all(parent)?; }
                            if from.metadata()?.len() > LARGE_FILE_THRESHOLD {
                                if copy_large_file_with_progress(&from, &to, &current_file_name, &tx, rx, total_sync_size, processed_size)? {
                                    return Ok(true); // Stopped
                                }
                            } else {
                                fs::copy(&from, &to)?;
                            }
                            format!("[{}] 冲突解决 (采用本地): {}", Local::now().format("%H:%M:%S"), path.display())
                        }
                        Resolution::KeepRemote => {
                            let from = usb_sync_path.join(&path);
                            let to = local_path.join(&path);
                            if let Some(parent) = to.parent() { fs::create_dir_all(parent)?; }
                            if from.metadata()?.len() > LARGE_FILE_THRESHOLD {
                                if copy_large_file_with_progress(&from, &to, &current_file_name, &tx, rx, total_sync_size, processed_size)? {
                                    return Ok(true); // Stopped
                                }
                            } else {
                                fs::copy(&from, &to)?;
                            }
                            format!("[{}] 冲突解决 (采用U盘): {}", Local::now().format("%H:%M:%S"), path.display())
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

        // After all operations, rescan to create the new, accurate metadata file.
        // Pass an empty SyncData to force re-hashing of all final files.
        tx.send(SyncMessage::Progress(0.99, "正在生成新的同步记录...".to_string()))?;
        let final_scan_result =
            scan_directory_with_progress(local_path, &tx, rx, local_total, "更新本地元数据", &SyncData::default())?;

        if let Some(mut final_sync_data) = final_scan_result {
            // Exclude skipped files from the new metadata so they are re-evaluated next time.
            final_sync_data.files.retain(|path, _| !skipped_files.contains(path));
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
        let msg = format!("[{}] 同步已由用户停止。", Local::now().format("%H:%M:%S"));
        tx.send(SyncMessage::Log(msg)).unwrap_or_default();
        tx.send(SyncMessage::Stopped).unwrap_or_default();
    } else {
        tx.send(SyncMessage::Complete).unwrap_or_default();
    }
}
