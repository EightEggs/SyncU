use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Clone)]
pub enum Resolution {
    KeepLocal,
    KeepRemote,
}

#[derive(Clone)]
pub enum SyncMessage {
    Log(String),
    ConfirmDeletion(PathBuf),
    DeletionConfirmed(bool),
    AskForConflictResolution {
        path: PathBuf,
        local_preview: String,
        remote_preview: String,
    },
    ConflictResolved(Resolution),
    Progress(f32, String),
    Complete,
    Stop,
    Stopped,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileInfo {
    pub path: PathBuf,
    pub hash: String,
    pub modified: SystemTime,
    #[serde(default)]
    pub size: u64,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct SyncData {
    pub files: HashMap<PathBuf, FileInfo>,
}

#[derive(Debug)]
pub enum SyncAction {
    Upload(PathBuf),
    Download(PathBuf),
    DeleteLocal(PathBuf),
    DeleteRemote(PathBuf),
    Conflict {
        path: PathBuf,
        local_preview: String,
        remote_preview: String,
    },
}