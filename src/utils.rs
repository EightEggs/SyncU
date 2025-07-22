use crate::models::{FileInfo, SyncData, SyncMessage};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;
use sysinfo::{DiskExt, System, SystemExt};
use walkdir::WalkDir;

pub fn find_usb_drives() -> Vec<PathBuf> {
    let mut sys = System::new_all();
    sys.refresh_disks_list();
    sys.disks()
        .iter()
        .filter(|d| d.is_removable())
        .map(|d| d.mount_point().to_path_buf())
        .collect()
}

pub fn scan_directory_with_progress(
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

            let progress = if total_files > 0 {
                processed_files as f32 / total_files as f32
            } else {
                1.0
            };
            let file_name = entry
                .path()
                .file_name()
                .unwrap_or_default()
                .to_str()
                .unwrap_or_default();
            tx.send(SyncMessage::Progress(
                progress,
                format!(
                    "{} ({}/{}) - {}",
                    ui_message_prefix, processed_files, total_files, file_name
                ),
            ))?;

            let metadata = fs::metadata(&path)?;
            let modified = metadata.modified()?;
            let size = metadata.len();
            let hash = if metadata.len() < 1_000_000 {
                // Only hash small files
                let mut file = File::open(&path)?;
                let mut hasher = Sha256::new();
                const BUFFER_SIZE: usize = 8192; // 8KB buffer
                let mut buffer = vec![0; BUFFER_SIZE];
                loop {
                    if let Ok(SyncMessage::Stop) = rx.try_recv() {
                        return Ok(None); // Stopped
                    }
                    let bytes_read = file.read(&mut buffer)?;
                    if bytes_read == 0 {
                        break;
                    }
                    hasher.update(&buffer[..bytes_read]);
                }
                format!("{:x}", hasher.finalize())
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

pub fn save_sync_data(sync_data: &SyncData, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_string_pretty(sync_data)?;
    fs::write(path, json)?;
    Ok(())
}

pub fn load_sync_data(path: &Path) -> Result<SyncData, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Ok(SyncData::default());
    }
    let json = fs::read_to_string(path)?;
    let sync_data = serde_json::from_str(&json)?;
    Ok(sync_data)
}

pub fn copy_large_file_with_progress(
    from: &Path,
    to: &Path,
    file_name_for_ui: &str,
    tx: &Sender<SyncMessage>,
    rx: &Receiver<SyncMessage>,
    total_sync_size: u64,
    processed_size_before: u64,
) -> Result<bool, Box<dyn std::error::Error>> {
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

        if total_sync_size > 0
            && (last_update.elapsed().as_millis() > 100 || bytes_copied == file_size)
        {
            let progress = (processed_size_before + bytes_copied) as f32 / total_sync_size as f32;
            tx.send(SyncMessage::Progress(
                progress,
                file_name_for_ui.to_string(),
            ))?;
            last_update = Instant::now();
        }
    }

    Ok(false)
}

pub fn write_log_entry(entry: &str, usb_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let log_path = usb_path.join("SyncU.log");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    writeln!(file, "{}", entry)?;
    Ok(())
}

pub fn get_file_preview(path: &Path) -> String {
    const PREVIEW_SIZE: usize = 1024; // 1KB
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return "[File not found]".to_string(),
    };

    let mut buffer = vec![0; PREVIEW_SIZE];
    let bytes_read = match file.read(&mut buffer) {
        Ok(n) => n,
        Err(_) => return "[Error reading file]".to_string(),
    };

    buffer.truncate(bytes_read);

    match String::from_utf8(buffer) {
        Ok(s) => s,
        Err(_) => "[Binary File - No Preview Available]".to_string(),
    }
}