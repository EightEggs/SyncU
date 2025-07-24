use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

/// Defines the user's choice when resolving a file conflict.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolution {
    KeepLocal,
    KeepRemote,
    Skip,
}

/// Defines the available UI themes.
#[derive(Clone, Debug, PartialEq)]
pub enum Theme {
    Light,
    Dark,
}

/// Messages passed between the UI thread and the synchronization thread.
#[derive(Clone, Debug, PartialEq)]
pub enum SyncMessage {
    // --- UI to Sync Thread ---
    /// Confirms or denies a deletion request from the sync thread.
    DeletionConfirmed(bool),
    /// Provides the resolution for a file conflict.
    ConflictResolved(Resolution),
    /// Signals the sync thread to stop its current operation.
    Stop,

    // --- Sync Thread to UI ---
    /// Sends a log message to be displayed in the UI.
    Log(String),
    /// Asks the user to confirm the deletion of a file.
    ConfirmDeletion(PathBuf),
    /// Asks the user to resolve a conflict between two file versions.
    AskForConflictResolution { path: PathBuf },
    /// Reports the progress of the current operation.
    Progress(f32, String),
    /// Indicates that the synchronization process has completed successfully.
    Complete,
    /// Indicates that the synchronization process was stopped by the user.
    Stopped,
}

/// Holds metadata about a single file for synchronization purposes.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileInfo {
    pub path: PathBuf,
    pub hash: String,
    pub modified: SystemTime,
    pub size: u64,
}

/// Represents the entire state of a synchronized directory, containing all file metadata.
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct SyncData {
    pub files: HashMap<PathBuf, FileInfo>,
}

/// Defines a specific synchronization action to be performed.
#[derive(Debug, Clone)]
pub enum SyncAction {
    LocalToRemote(PathBuf),
    RemoteToLocal(PathBuf),
    DeleteLocal(PathBuf),
    DeleteRemote(PathBuf),
    Conflict { path: PathBuf },
}
