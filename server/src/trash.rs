//! The Trash. A deleted item is moved to `<root>/.narwhal-trash/files/<id>`,
//! and `info/<id>.json` records where it came from and when. The folder sits
//! on the same volume as everything else, so trashing and restoring are
//! cheap renames. API paths can never reach it (see `fsops::resolve`).

use crate::error::{ApiError, ApiResult};
use crate::fsops::{self, components, entry_for, normalize_virtual, remove_any, rename_noreplace};
use narwhal_shared::{vjoin, vname, vparent, OpReport, TrashItem};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Name of the Trash folder in the root. Reserved: nothing else may use it.
pub const DIR: &str = ".narwhal-trash";

#[derive(Serialize, Deserialize)]
struct Meta {
    /// Original virtual path.
    path: String,
    /// Seconds since the Unix epoch.
    deleted: i64,
}

static SEQ: AtomicU64 = AtomicU64::new(0);

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Ids are generated here; anything else from a client is refused, so an id
/// can never name a path outside the Trash.
fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn check_id(id: &str) -> ApiResult<()> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(ApiError::bad_request("Invalid Trash item"))
    }
}

fn base(root: &Path) -> PathBuf {
    root.join(DIR)
}

/// The `files` and `info` folders, created if needed. Refuses a Trash
/// folder that is a symlink, which could point anywhere.
fn dirs(root: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let base = base(root);
    match fs::symlink_metadata(&base) {
        Ok(m) if !m.is_dir() => {
            return Err(io::Error::other(format!("{DIR} in the storage root is not a folder")));
        }
        Ok(_) => {}
        Err(_) => fs::create_dir(&base)?,
    }
    let files = base.join("files");
    let info = base.join("info");
    for d in [&files, &info] {
        match fs::symlink_metadata(d) {
            Ok(m) if !m.is_dir() => return Err(io::Error::other("The Trash folder is damaged")),
            Ok(_) => {}
            Err(_) => fs::create_dir(d)?,
        }
    }
    Ok((files, info))
}

fn read_meta(info: &Path, id: &str) -> io::Result<Meta> {
    let raw = fs::read(info.join(format!("{id}.json")))?;
    serde_json::from_slice(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Ids of everything listed in `info`.
fn ids(info: &Path) -> Vec<String> {
    fs::read_dir(info)
        .map(|rd| {
            rd.flatten()
                .filter_map(|de| de.file_name().to_str()?.strip_suffix(".json").map(str::to_string))
                .filter(|id| valid_id(id))
                .collect()
        })
        .unwrap_or_default()
}

fn label(vp: &str) -> &str {
    match vname(vp) {
        "" => "/",
        n => n,
    }
}

/// Number of items in the Trash (without creating it).
pub fn count(root: &Path) -> u32 {
    ids(&base(root).join("info")).len() as u32
}

/// Move items to the Trash. `ids` in the report lists their Trash ids.
pub fn trash(root: &Path, paths: &[String]) -> OpReport {
    let mut report = OpReport::default();
    let (files, info) = match dirs(root) {
        Ok(d) => d,
        Err(e) => {
            report.errors.push(format!("Could not open the Trash: {}", ApiError::from(e).message));
            return report;
        }
    };
    for vp in paths {
        match trash_one(root, &files, &info, vp) {
            Ok(id) => {
                report.done += 1;
                report.ids.push(id);
            }
            Err(e) => report.errors.push(format!("{}: {}", label(vp), e.message)),
        }
    }
    report
}

fn trash_one(root: &Path, files: &Path, info: &Path, vp: &str) -> ApiResult<String> {
    let src = fsops::resolve(root, vp)?;
    if src == root {
        return Err(ApiError::forbidden("The root folder cannot be deleted"));
    }
    fs::symlink_metadata(&src)?;
    let meta = serde_json::to_vec(&Meta { path: normalize_virtual(vp)?, deleted: now() })
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Reserve a fresh id by creating its info file.
    let (id, info_path) = loop {
        let id = format!("{}-{}-{}", now(), std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed));
        let path = info.join(format!("{id}.json"));
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                f.write_all(&meta)?;
                break (id, path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    };
    if let Err(e) = rename_noreplace(&src, &files.join(&id)) {
        let _ = fs::remove_file(&info_path);
        if e.raw_os_error() == Some(libc::EXDEV) {
            return Err(ApiError::bad_request(
                "It is on a different volume than the Trash; use Delete Permanently (Shift+Del)",
            ));
        }
        return Err(e.into());
    }
    Ok(id)
}

/// Everything in the Trash, most recently deleted first.
pub fn list(root: &Path) -> ApiResult<Vec<TrashItem>> {
    let base = base(root);
    let (files, info) = (base.join("files"), base.join("info"));
    let mut out = Vec::new();
    for id in ids(&info) {
        let Ok(meta) = read_meta(&info, &id) else { continue };
        match entry_for(&files.join(&id), vname(&meta.path).to_string(), meta.path) {
            Some(entry) => out.push(TrashItem { id, entry, deleted: meta.deleted }),
            // The item itself is gone: drop the stale record.
            None => {
                let _ = fs::remove_file(info.join(format!("{id}.json")));
            }
        }
    }
    out.sort_by_key(|t| std::cmp::Reverse(t.deleted));
    Ok(out)
}

/// Put items back where they were. Missing folders are recreated, and a
/// name that has been taken meanwhile gets a " (1)" suffix. `ids` in the
/// report lists the restored paths.
pub fn restore(root: &Path, ids: &[String]) -> OpReport {
    let mut report = OpReport::default();
    let base = base(root);
    let (files, info) = (base.join("files"), base.join("info"));
    for id in ids {
        let mut name = id.clone();
        let res = (|| -> ApiResult<String> {
            check_id(id)?;
            let meta = read_meta(&info, id)?;
            name = vname(&meta.path).to_string();
            let src = files.join(id);
            fs::symlink_metadata(&src)?;
            let vdir = vparent(&meta.path);
            let dir = fsops::make_upload_dirs(root, root, &components(&vdir)?)?;
            let target = fsops::rename_unique(&src, &dir, &name)?;
            let _ = fs::remove_file(info.join(format!("{id}.json")));
            let new_name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(name.clone());
            Ok(vjoin(&vdir, &new_name))
        })();
        match res {
            Ok(path) => {
                report.done += 1;
                report.ids.push(path);
            }
            Err(e) => report.errors.push(format!("{name}: {}", e.message)),
        }
    }
    report
}

fn purge_one(files: &Path, info: &Path, id: &str) -> ApiResult<()> {
    check_id(id)?;
    match remove_any(&files.join(id)) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let _ = fs::remove_file(info.join(format!("{id}.json")));
    Ok(())
}

/// Delete items from the Trash for good.
pub fn purge(root: &Path, ids: &[String]) -> OpReport {
    let mut report = OpReport::default();
    let base = base(root);
    let (files, info) = (base.join("files"), base.join("info"));
    for id in ids {
        match purge_one(&files, &info, id) {
            Ok(()) => report.done += 1,
            Err(e) => {
                let name = read_meta(&info, id).map(|m| vname(&m.path).to_string()).unwrap_or(id.clone());
                report.errors.push(format!("{name}: {}", e.message));
            }
        }
    }
    report
}

/// Delete everything in the Trash, including leftovers without a record.
pub fn empty(root: &Path) -> OpReport {
    let base = base(root);
    let (files, info) = (base.join("files"), base.join("info"));
    let mut report = purge(root, &ids(&info));
    if let Ok(rd) = fs::read_dir(&files) {
        for de in rd.flatten() {
            if let Err(e) = remove_any(&de.path()) {
                report.errors.push(format!("{}: {}", de.file_name().to_string_lossy(), ApiError::from(e).message));
            }
        }
    }
    report
}

/// Delete items that have been in the Trash for more than `days` days.
pub fn expire(root: &Path, days: u64) -> OpReport {
    let cutoff = now() - (days as i64) * 86_400;
    let info = base(root).join("info");
    let old: Vec<String> = ids(&info)
        .into_iter()
        .filter(|id| read_meta(&info, id).map(|m| m.deleted < cutoff).unwrap_or(false))
        .collect();
    purge(root, &old)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("docs/sub")).unwrap();
        fs::write(root.join("docs/a.txt"), b"hello").unwrap();
        fs::write(root.join("docs/sub/b.txt"), b"world!").unwrap();
        (tmp, root)
    }

    #[test]
    fn trash_and_restore() {
        let (_t, root) = setup();
        let r = trash(&root, &["/docs/a.txt".into(), "/docs/sub".into()]);
        assert_eq!(r.done, 2, "{:?}", r.errors);
        assert!(!root.join("docs/a.txt").exists());
        assert!(!root.join("docs/sub").exists());
        assert_eq!(count(&root), 2);

        let items = list(&root).unwrap();
        assert_eq!(items.len(), 2);
        let file = items.iter().find(|i| i.entry.name == "a.txt").unwrap();
        assert_eq!(file.entry.path, "/docs/a.txt");
        assert_eq!(file.entry.size, 5);

        // The name was taken meanwhile: restore next to it.
        fs::write(root.join("docs/a.txt"), b"new").unwrap();
        let r = restore(&root, std::slice::from_ref(&file.id));
        assert_eq!(r.ids, vec!["/docs/a (1).txt".to_string()]);
        assert_eq!(fs::read(root.join("docs/a (1).txt")).unwrap(), b"hello");
        assert_eq!(count(&root), 1);
    }

    #[test]
    fn restore_recreates_missing_folders() {
        let (_t, root) = setup();
        let r = trash(&root, &["/docs/sub/b.txt".into()]);
        fs::remove_dir_all(root.join("docs")).unwrap();
        let r = restore(&root, &r.ids);
        assert_eq!(r.done, 1, "{:?}", r.errors);
        assert_eq!(fs::read(root.join("docs/sub/b.txt")).unwrap(), b"world!");
    }

    #[test]
    fn purge_empty_and_expire() {
        let (_t, root) = setup();
        let r = trash(&root, &["/docs/a.txt".into()]);
        assert_eq!(purge(&root, &r.ids).done, 1);
        assert_eq!(count(&root), 0);
        assert!(fs::read_dir(root.join(DIR).join("files")).unwrap().next().is_none());

        trash(&root, &["/docs/sub".into()]);
        assert_eq!(expire(&root, 30).done, 0);
        assert_eq!(count(&root), 1);
        empty(&root);
        assert_eq!(count(&root), 0);
        assert!(list(&root).unwrap().is_empty());
    }

    #[test]
    fn trash_is_unreachable_and_ids_are_checked() {
        let (_t, root) = setup();
        trash(&root, &["/docs/a.txt".into()]);
        assert!(fsops::resolve(&root, &format!("/{DIR}/files")).is_err());
        assert!(fsops::list_dir(&root, "/").unwrap().entries.iter().all(|e| e.name != DIR));
        assert!(fsops::sanitize_name(DIR).is_err());
        assert!(restore(&root, &["../../docs".into()]).errors.len() == 1);
        assert!(purge(&root, &["../docs".into()]).errors.len() == 1);
        assert!(root.join("docs").exists());
        assert!(trash(&root, &["/".into()]).errors.len() == 1);
    }
}
