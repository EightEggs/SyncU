use crate::models::{FileInfo, SyncData, SyncMessage};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;
use sysinfo::{DiskExt, System, SystemExt};
use walkdir::WalkDir;

/// Finds all removable drives connected to the system.
pub fn find_usb_drives() -> Vec<PathBuf> {
    let mut sys = System::new_all();
    sys.refresh_disks_list();
    sys.disks()
        .iter()
        .filter(|d| d.is_removable())
        .map(|d| d.mount_point().to_path_buf())
        .collect()
}

/// Calculates the SHA256 hash of a file.
fn calculate_hash(
    path: &Path,
    rx: &Receiver<SyncMessage>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 8192]; // 8KB buffer
    loop {
        // Check for stop signal periodically to avoid blocking
        if let Ok(SyncMessage::Stop) = rx.try_recv() {
            return Ok(None);
        }
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }
    Ok(Some(format!("{:x}", hasher.finalize())))
}

/// Scans a directory, calculates file hashes incrementally, and sends progress updates.
/// Skips hashing for files whose size and modification date haven't changed since the last sync.
pub fn scan_directory_with_progress(
    base_path: &Path,
    tx: &Sender<SyncMessage>,
    rx: &Receiver<SyncMessage>,
    total_files: usize,
    ui_message_prefix: &str,
    last_sync_data: &SyncData,
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
            let path = entry.path();
            let file_name = path.file_name().unwrap_or_default().to_str().unwrap_or_default();

            // Ignore metadata and log files
            if file_name == ".syncu_metadata.json" || file_name == ".syncu_log.txt" {
                continue;
            }

            let relative_path = path.strip_prefix(base_path)?.to_path_buf();
            let metadata = fs::metadata(path)?;
            let modified = metadata.modified()?;
            let size = metadata.len();

            // Send progress update to the UI
            let progress = if total_files > 0 {
                processed_files as f32 / total_files as f32
            } else {
                1.0
            };
            tx.send(SyncMessage::Progress(
                progress,
                format!(
                    "{} ({}/{}) - {}",
                    ui_message_prefix, processed_files, total_files, file_name
                ),
            ))?;

            // --- PERFORMANCE OPTIMIZATION ---
            // Check if the file needs to be re-hashed. If size and modification time are the same,
            // we can reuse the old hash.
            let hash = if let Some(last_file_info) = last_sync_data.files.get(&relative_path) {
                if last_file_info.modified == modified && last_file_info.size == size {
                    // File metadata matches, reuse the existing hash.
                    last_file_info.hash.clone()
                } else {
                    // File has changed, a new hash is required.
                    match calculate_hash(path, rx)? {
                        Some(h) => h,
                        None => return Ok(None), // Stop signal received during hashing
                    }
                }
            } else {
                // It's a new file, so we must calculate the hash.
                match calculate_hash(path, rx)? {
                    Some(h) => h,
                    None => return Ok(None), // Stop signal received during hashing
                }
            };

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

/// Saves the synchronization metadata to a JSON file.
pub fn save_sync_data(sync_data: &SyncData, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::create(path)?;
    serde_json::to_writer_pretty(file, sync_data)?;
    Ok(())
}

/// Loads synchronization metadata from a JSON file.
pub fn load_sync_data(path: &Path) -> Result<SyncData, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Ok(SyncData::default());
    }
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let sync_data = serde_json::from_reader(reader)?;
    Ok(sync_data)
}

/// Copies a large file with progress reporting, allowing for cancellation.
pub fn copy_large_file_with_progress(
    from: &Path,
    to: &Path,
    file_name_for_ui: &str,
    tx: &Sender<SyncMessage>,
    rx: &Receiver<SyncMessage>,
    total_sync_size: u64,
    processed_size_before: u64,
) -> Result<bool, io::Error> {
    let file_size = fs::metadata(from)?.len();
    let mut source = File::open(from)?;
    let mut dest = File::create(to)?;
    let mut buffer = vec![0; 64 * 1024]; // 64KB buffer
    let mut copied_size = 0;
    let mut last_update = Instant::now();

    loop {
        if let Ok(SyncMessage::Stop) = rx.try_recv() {
            // Clean up the partially copied file on cancellation
            drop(dest);
            fs::remove_file(to)?;
            return Ok(true);
        }

        let bytes_read = source.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        dest.write_all(&buffer[..bytes_read])?;
        copied_size += bytes_read as u64;

        // Throttle progress updates to avoid overwhelming the UI thread
        if total_sync_size > 0 && (last_update.elapsed().as_millis() > 50 || copied_size == file_size) {
            let progress = (processed_size_before + copied_size) as f32 / total_sync_size as f32;
            let file_progress = copied_size as f32 / file_size as f32;
            tx.send(SyncMessage::Progress(
                progress,
                format!(
                    "复制: {} ({:.0}%)",
                    file_name_for_ui,
                    file_progress * 100.0
                ),
            ))
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "Failed to send progress"))?;
            last_update = Instant::now();
        }
    }
    Ok(false)
}

/// Writes a log message to the .syncu_log.txt file in the sync directory.
pub fn write_log_entry(message: &str, usb_sync_path: &Path) -> Result<(), io::Error> {
    let log_path = usb_sync_path.join(".syncu_log.txt");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    writeln!(file, "{}", message)?;
    Ok(())
}
