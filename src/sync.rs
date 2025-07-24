use crate::models::{Resolution, SyncAction, SyncData, SyncMessage};
use crate::utils::{copy_large_file_with_progress, load_sync_data, save_sync_data, scan_directory_with_progress, write_log_entry};
use chrono::Local;
use crossbeam_channel::Receiver;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use walkdir::WalkDir;

const LARGE_FILE_THRESHOLD: u64 = 10 * 1024 * 1024; // 10 MB

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
        )).map_err(|_| "发送消息失败")?;
        let last_sync_data = load_sync_data(&metadata_path)?;

        // --- PERFORMANCE OPTIMIZATION: Pass last_sync_data to avoid re-hashing everything ---
        tx.send(SyncMessage::Progress(0.0, "正在统计本地文件...".to_string())).map_err(|_| "发送消息失败")?;
        let local_total = WalkDir::new(local_path).into_iter().filter_map(Result::ok).filter(|e| e.file_type().is_file()).count();
        let local_sync_data =
            match scan_directory_with_progress(local_path, &tx, &rx, local_total, "扫描本地", &last_sync_data)? {
                Some(data) => data,
                None => return Ok(true), // Stopped
            };

        tx.send(SyncMessage::Progress(0.0, "正在统计U盘文件...".to_string())).map_err(|_| "发送消息失败")?;
        let remote_total = WalkDir::new(&usb_sync_path).into_iter().filter_map(Result::ok).filter(|e| e.file_type().is_file()).count();
        let remote_sync_data =
            match scan_directory_with_progress(&usb_sync_path, &tx, &rx, remote_total, "扫描U盘", &last_sync_data)?
            {
                Some(data) => data,
                None => return Ok(true), // Stopped
            };

        // --- UNIFIED SYNC PLAN: Build a single, correct plan of action ---
        tx.send(SyncMessage::Progress(0.0, "正在分析文件差异...".to_string())).map_err(|_| "发送消息失败")?;

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
                // Case 1: File exists on all three. Check for changes.
                (Some(local), Some(remote), Some(last)) => {
                    let local_changed = local.hash != last.hash;
                    let remote_changed = remote.hash != last.hash;
                    if local_changed && remote_changed {
                        Some(SyncAction::Conflict { path: path.clone() })
                    } else if local_changed {
                        Some(SyncAction::LocalToRemote(path.clone()))
                    } else if remote_changed {
                        Some(SyncAction::RemoteToLocal(path.clone()))
                    } else {
                        None // No changes needed.
                    }
                }
                // Case 2: File added locally and remotely since last sync.
                (Some(local), Some(remote), None) => {
                    if local.hash == remote.hash {
                        None // Files are identical, no action needed.
                    } else {
                        Some(SyncAction::Conflict { path: path.clone() })
                    }
                }
                // Case 3: File deleted from remote.
                (Some(_), None, Some(_)) => Some(SyncAction::DeleteLocal(path.clone())),
                // Case 4: File deleted locally.
                (None, Some(_), Some(_)) => Some(SyncAction::DeleteRemote(path.clone())),
                // Case 5: New file added locally.
                (Some(_), None, None) => Some(SyncAction::LocalToRemote(path.clone())),
                // Case 6: New file added on remote.
                (None, Some(_), None) => Some(SyncAction::RemoteToLocal(path.clone())),
                // Case 7: File deleted from both.
                (None, None, Some(_)) => None,
                // Case 8: Path exists only in memory but not on disk, ignore.
                (None, None, None) => None,
            };

            if let Some(action) = action {
                sync_plan.push(action);
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
            tx.send(SyncMessage::Log("未检测到变化.".to_owned())).map_err(|_| "发送消息失败")?;
        } else {
            tx.send(SyncMessage::Log(format!("计划执行 {} 个同步操作...", sync_plan_len))).map_err(|_| "发送消息失败")?;
        }

        // --- CONCURRENT PROCESSING: Process sync actions in batches ---
        // We'll process actions in smaller batches to allow for better concurrency
        const BATCH_SIZE: usize = 5;
        let mut batch_start = 0;
        
        while batch_start < sync_plan.len() {
            if rx.try_recv() == Ok(SyncMessage::Stop) {
                return Ok(true);
            }
            
            let batch_end = std::cmp::min(batch_start + BATCH_SIZE, sync_plan.len());
            let batch = &sync_plan[batch_start..batch_end];
            
            // Process batch actions
            for (i, action) in batch.iter().enumerate() {
                let index = batch_start + i;
                
                if rx.try_recv() == Ok(SyncMessage::Stop) {
                    return Ok(true);
                }

                let (file_size, current_file_name) = match action {
                    SyncAction::LocalToRemote(path) | SyncAction::RemoteToLocal(path) | SyncAction::Conflict { path, .. } => {
                        let full_path = if matches!(action, SyncAction::RemoteToLocal(_)) { usb_sync_path.join(path) } else { local_path.join(path) };
                        (fs::metadata(full_path).map(|m| m.len()).unwrap_or(0), path.to_str().unwrap_or("").to_string())
                    }
                    SyncAction::DeleteLocal(path) | SyncAction::DeleteRemote(path) => {
                        (0, format!("删除: {}", path.to_str().unwrap_or("")))
                    }
                };

                let progress = if total_sync_size > 0 { processed_size as f32 / total_sync_size as f32 } else { 0.0 };
                tx.send(SyncMessage::Progress(progress, format!("({}/{})正在处理: {}", index + 1, sync_plan_len, current_file_name))).map_err(|_| "发送消息失败")?;

                let message = match action {
                    SyncAction::LocalToRemote(path) => {
                        let from = local_path.join(path);
                        let to = usb_sync_path.join(path);
                        if let Some(parent) = to.parent() { fs::create_dir_all(parent)?; }
                        if fs::metadata(&from)?.len() > LARGE_FILE_THRESHOLD {
                            if copy_large_file_with_progress(&from, &to, &current_file_name, &tx, &rx, total_sync_size, processed_size)? {
                                return Ok(true); // Stopped
                            }
                        } else {
                            fs::copy(&from, &to)?;
                        }
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
                        } else {
                            fs::copy(&from, &to)?;
                        }
                        format!("[{}] U盘 -> 本地: {}", Local::now().format("%H:%M:%S"), from.strip_prefix(&usb_sync_path)?.display())
                    }
                    SyncAction::DeleteRemote(path) => {
                        let absolute_path = usb_sync_path.join(path);
                        tx.send(SyncMessage::ConfirmDeletion(absolute_path.clone())).map_err(|_| "发送消息失败")?;
                        let confirmed = loop {
                            match rx.recv() {
                                Ok(SyncMessage::DeletionConfirmed(c)) => break c,
                                Ok(SyncMessage::Stop) => return Ok(true),
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
                        let absolute_path = local_path.join(path);
                        tx.send(SyncMessage::ConfirmDeletion(absolute_path.clone())).map_err(|_| "发送消息失败")?;
                        let confirmed = loop {
                            match rx.recv() {
                                Ok(SyncMessage::DeletionConfirmed(c)) => break c,
                                Ok(SyncMessage::Stop) => return Ok(true),
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
                        tx.send(SyncMessage::AskForConflictResolution { path: path.clone() }).map_err(|_| "发送消息失败")?;
                        let resolution = loop {
                            match rx.recv() {
                                Ok(SyncMessage::ConflictResolved(r)) => break r,
                                Ok(SyncMessage::Stop) => return Ok(true),
                                _ => {}
                            }
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
                                } else {
                                    fs::copy(&from, &to)?;
                                }
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
                                } else {
                                    fs::copy(&from, &to)?;
                                }
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
                tx.send(SyncMessage::Log(message.clone())).map_err(|_| "发送消息失败")?;
                write_log_entry(&message, &usb_sync_path)?;
            }
            
            batch_start = batch_end;
        }

        // After all operations, rescan to create the new, accurate metadata file.
        // Pass an empty SyncData to force re-hashing of all final files.
        tx.send(SyncMessage::Progress(0.99, "正在生成新的同步记录...".to_string())).map_err(|_| "发送消息失败")?;
        let final_scan_result =
            scan_directory_with_progress(local_path, &tx, &rx, local_total, "更新本地元数据", &SyncData::default())?;

        if let Some(mut final_sync_data) = final_scan_result {
            // Exclude skipped files from the new metadata so they are re-evaluated next time.
            final_sync_data.files.retain(|path, _| !skipped_files.contains(path));
            save_sync_data(&final_sync_data, &metadata_path)?;
        }

        tx.send(SyncMessage::Progress(1.0, "同步完成!".to_string())).map_err(|_| "发送消息失败")?;
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
