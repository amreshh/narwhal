//! Types shared between the Axum server and the Leptos (WASM) frontend.
//!
//! All paths exchanged over the API are *virtual* paths: they always start
//! with `/` and are relative to the server's configured root directory.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Dir,
    File,
    Other,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    /// Virtual path, e.g. `/photos/2024/beach.jpg`.
    pub path: String,
    pub kind: EntryKind,
    /// Size in bytes (0 for directories).
    pub size: u64,
    /// Number of entries inside a directory (None for files).
    pub children: Option<u32>,
    /// Last modification time, seconds since the Unix epoch.
    pub modified: Option<i64>,
    pub symlink: bool,
    pub hidden: bool,
    /// Unix permission bits (e.g. 0o644), if available.
    pub mode: Option<u32>,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Listing {
    pub path: String,
    pub entries: Vec<Entry>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct DiskInfo {
    pub total: u64,
    pub available: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Place {
    pub name: String,
    pub path: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Info {
    /// Display name of the root volume.
    pub name: String,
    pub read_only: bool,
    pub disk: Option<DiskInfo>,
    pub places: Vec<Place>,
    /// Deleting moves items to the Trash (instead of removing them).
    #[serde(default)]
    pub trash: bool,
    /// Number of items in the Trash.
    #[serde(default)]
    pub trash_items: u32,
    /// Default number of files uploaded at the same time.
    #[serde(default)]
    pub upload_parallel: u32,
    /// The release this server is (e.g. `1.2.3`); none for other builds.
    #[serde(default)]
    pub version: Option<String>,
    /// The git commit it was built from, in full.
    #[serde(default)]
    pub commit: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CreateReq {
    /// Parent directory.
    pub path: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RenameReq {
    pub path: String,
    pub new_name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DeleteReq {
    pub paths: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TransferOp {
    Copy,
    Move,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TransferReq {
    pub paths: Vec<String>,
    /// Destination directory.
    pub dest: String,
    pub op: TransferOp,
}

/// Result of a batch operation; `errors` lists per-item failures.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct OpReport {
    pub done: u32,
    pub errors: Vec<String>,
    /// What the operation produced, where useful: Trash ids for a move to
    /// the Trash, restored paths for a restore.
    #[serde(default)]
    pub ids: Vec<String>,
}

/// One item in the Trash.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TrashItem {
    pub id: String,
    /// The item as it is now, with its original name and path.
    pub entry: Entry,
    /// When it was deleted, seconds since the Unix epoch.
    pub deleted: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TrashReq {
    pub ids: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
pub struct SizeReport {
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ErrorBody {
    pub error: String,
}

/// Join a virtual directory path and a child name.
pub fn vjoin(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Parent of a virtual path (`/a/b` -> `/a`, `/a` -> `/`, `/` -> `/`).
pub fn vparent(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => trimmed[..i].to_string(),
    }
}

/// Last component of a virtual path.
pub fn vname(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(vjoin("/", "a"), "/a");
        assert_eq!(vjoin("/a", "b"), "/a/b");
        assert_eq!(vparent("/a/b"), "/a");
        assert_eq!(vparent("/a"), "/");
        assert_eq!(vparent("/"), "/");
        assert_eq!(vname("/a/b.txt"), "b.txt");
        assert_eq!(vname("/"), "");
    }
}
