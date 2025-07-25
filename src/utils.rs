use crate::models::{FileInfo, SyncData, SyncMessage};
use crossbeam_channel::Receiver;
use dashmap::{DashMap, DashSet};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};
use std::time::Instant;
use sysinfo::{System, Disks};
use walkdir::WalkDir;

/// Finds all removable drives connected to the system.
pub fn find_usb_drives() -> Vec<PathBuf> {
    let mut sys = System::new();
    sys.refresh_all();
    let disks = Disks::new_with_refreshed_list();
    disks
        .iter()
        .filter(|d| d.is_removable())
        .map(|d| d.mount_point().to_path_buf())
        .collect()
}

/// Calculates the SHA256 hash of a file.
fn calculate_hash(
    path: &Path,
    stop_flag: &AtomicBool,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 8192]; // 8KB buffer
    loop {
        // Check for stop signal periodically to avoid blocking
        if stop_flag.load(Ordering::Relaxed) {
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
    tx: &crossbeam_channel::Sender<SyncMessage>,
    rx: &Receiver<SyncMessage>,
    total_entries: usize,
    ui_message_prefix: &str,
    last_sync_data: &SyncData,
) -> Result<Option<SyncData>, Box<dyn std::error::Error>> {
    let files = DashMap::new();
    let directories = DashSet::new();
    let processed_entries = AtomicUsize::new(0);
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Collect all entries first
    let entries: Vec<_> = WalkDir::new(base_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .collect();

    // Process entries in parallel
    let results: Vec<_> = entries
        .par_iter()
        .map(|entry| {
            // Check for stop signal from the UI thread
            if let Ok(SyncMessage::Stop) = rx.try_recv() {
                stop_flag.store(true, Ordering::Relaxed);
            }
            if stop_flag.load(Ordering::Relaxed) {
                return None;
            }

            let path = entry.path();
            let file_name = path.file_name().unwrap_or_default().to_str().unwrap_or_default();

            // Ignore metadata and log files
            if file_name == ".syncu_metadata.json" || file_name == ".syncu_log.txt" {
                return None;
            }

            let relative_path = match path.strip_prefix(base_path) {
                Ok(p) => p.to_path_buf(),
                Err(_) => return None,
            };
            
            if relative_path.as_os_str().is_empty() {
                return None; // Skip the root directory itself
            }

            // Update progress counter
            let current_processed = processed_entries.fetch_add(1, Ordering::Relaxed) + 1;
            
            if current_processed % 10 == 1 {
                let progress = if total_entries > 0 {
                    current_processed as f32 / total_entries as f32
                } else {
                    1.0
                };
                
                let _ = tx.send(SyncMessage::Progress(
                    progress,
                    format!(
                        "{} ({}/{}) - {}",
                        ui_message_prefix, current_processed, total_entries, file_name
                    ),
                ));
            }

            if entry.file_type().is_dir() {
                directories.insert(relative_path);
                return None; // Return None for directories as they don't need further processing in this map
            }

            // From here, we are dealing with a file
            let metadata = match fs::metadata(path) {
                Ok(m) => m,
                Err(_) => return None,
            };

            let modified = match metadata.modified() {
                Ok(m) => m,
                Err(_) => return None,
            };

            let size = metadata.len();

            let hash = if let Some(last_file_info) = last_sync_data.files.get(&relative_path) {
                if last_file_info.modified == modified && last_file_info.size == size {
                    last_file_info.hash.clone()
                } else {
                    match calculate_hash(path, &stop_flag) {
                        Ok(Some(h)) => h,
                        Ok(None) => return None,
                        Err(_) => return None,
                    }
                }
            } else {
                match calculate_hash(path, &stop_flag) {
                    Ok(Some(h)) => h,
                    Ok(None) => return None,
                    Err(_) => return None,
                }
            };

            Some((
                relative_path.clone(),
                FileInfo {
                    path: relative_path,
                    hash,
                    modified,
                    size,
                },
            ))
        })
        .collect();

    if stop_flag.load(Ordering::Relaxed) {
        return Ok(None);
    }

    for result in results {
        if let Some((path, info)) = result {
            files.insert(path, info);
        }
    }

    let files_map: HashMap<PathBuf, FileInfo> = files.into_iter().collect();
    let directories_set: HashSet<PathBuf> = directories.into_iter().collect();
    
    Ok(Some(SyncData {
        files: files_map,
        directories: directories_set,
    }))
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
    tx: &crossbeam_channel::Sender<SyncMessage>,
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
        match rx.try_recv() {
            Ok(SyncMessage::Stop) => {
                // Clean up the partially copied file on cancellation
                drop(dest);
                let _ = fs::remove_file(to);
                return Ok(true);
            }
            _ => {}
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
                    "正在处理: {} ({:.0}%)",
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

/// Helper function to remove ancestor paths.
/// If we have {"a", "a/b"}, it returns {"a/b"}.
pub fn prune_ancestor_paths(paths: &HashSet<PathBuf>) -> HashSet<PathBuf> {
    let mut pruned = paths.clone();
    pruned.retain(|p1| !paths.iter().any(|p2| p1 != p2 && p2.starts_with(p1)));
    pruned
}

/// Helper function to remove descendant paths.
/// If we have {"a", "a/b"}, it returns {"a"}.
pub fn prune_descendant_paths(paths: &HashSet<PathBuf>) -> HashSet<PathBuf> {
    let mut pruned = paths.clone();
    pruned.retain(|p1| !paths.iter().any(|p2| p1 != p2 && p1.starts_with(p2)));
    pruned
}

/// Recursively cleans up empty parent directories.
pub fn cleanup_empty_dirs(start_path: &Path, base_path: &Path) -> Result<(), std::io::Error> {
    let mut current = start_path.parent();
    while let Some(dir) = current {
        if !dir.starts_with(base_path) || dir == base_path {
            break;
        }
        // Check if the directory is empty
        if dir.read_dir()?.next().is_none() {
            fs::remove_dir(dir)?;
        } else {
            // Stop if we find a non-empty directory
            break;
        }
        current = dir.parent();
    }
    Ok(())
}
