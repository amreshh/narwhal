//! The Trash. A deleted item is moved to `<root>/.narwhal-trash/files/<id>`,
//! and `info/<id>.json` records where it came from and when. The folder sits
//! on the same volume as everything else, so trashing and restoring are
//! cheap renames. API paths can never reach it (see `confined::Root`), and
//! its folders are opened without following symlinks, so it can't be
//! redirected somewhere else either.

use crate::confined::{Dir, Root};
use crate::error::{ApiError, ApiResult};
use crate::fsops::{self, check_name, components, entry_nofollow, normalize_virtual, now, rename_unique};
use narwhal_shared::{vjoin, vname, vparent, OpReport, TrashItem};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};

/// Name of the Trash folder in the root. Reserved: nothing else may use it.
pub const DIR: &str = ".narwhal-trash";

/// A record younger than this is never treated as stale: its item may be
/// moving into the Trash right now.
const RECORD_GRACE_SECS: i64 = 300;

#[derive(Serialize, Deserialize)]
struct Meta {
    /// Original virtual path.
    path: String,
    /// Seconds since the Unix epoch.
    deleted: i64,
}

static SEQ: AtomicU64 = AtomicU64::new(0);

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

fn record(id: &str) -> OsString {
    OsString::from(format!("{id}.json"))
}

/// The `files` and `info` folders. With `create`, missing ones are created;
/// otherwise None means there is no Trash yet. A symlink (or a file) in
/// place of one of them is refused: it could lead anywhere.
fn dirs(root: &Root, create: bool) -> io::Result<Option<(Dir, Dir)>> {
    let open = |parent: &Dir, name: &str| -> io::Result<Option<Dir>> {
        let name = OsStr::new(name);
        if create {
            match parent.mkdir(name, 0o777) {
                Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
                _ => {}
            }
        }
        match parent.open_dir(name) {
            Ok(d) => Ok(Some(d)),
            Err(e) if e.kind() == io::ErrorKind::NotFound && !create => Ok(None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(e),
            Err(_) => Err(io::Error::other(format!("{DIR} in the storage root is damaged (not a folder)"))),
        }
    };
    let Some(base) = open(root.dir(), DIR)? else { return Ok(None) };
    let (Some(files), Some(info)) = (open(&base, "files")?, open(&base, "info")?) else { return Ok(None) };
    Ok(Some((files, info)))
}

fn read_meta(info: &Dir, id: &str) -> io::Result<Meta> {
    let mut raw = Vec::new();
    info.open_file(&record(id))?.read_to_end(&mut raw)?;
    serde_json::from_slice(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Ids of everything listed in `info`.
fn ids(info: &Dir) -> Vec<String> {
    info.entries()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(name, _)| name.to_str()?.strip_suffix(".json").map(str::to_string))
        .filter(|id| valid_id(id))
        .collect()
}

fn label(vp: &str) -> &str {
    match vname(vp) {
        "" => "/",
        n => n,
    }
}

/// Number of items in the Trash (without creating it).
pub fn count(root: &Root) -> u32 {
    match dirs(root, false) {
        Ok(Some((_, info))) => ids(&info).len() as u32,
        _ => 0,
    }
}

/// Move items to the Trash. `ids` in the report lists their Trash ids.
pub fn trash(root: &Root, paths: &[String]) -> OpReport {
    let mut report = OpReport::default();
    let (files, info) = match dirs(root, true) {
        Ok(Some(d)) => d,
        res => {
            let why = res.err().map(|e| ApiError::from(e).message).unwrap_or_default();
            report.errors.push(format!("Could not open the Trash: {why}"));
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

fn trash_one(root: &Root, files: &Dir, info: &Dir, vp: &str) -> ApiResult<String> {
    let Some((dir, name)) = fsops::locate(root, vp)? else {
        return Err(ApiError::forbidden("The root folder cannot be deleted"));
    };
    dir.lstat(&name)?;
    let meta = serde_json::to_vec(&Meta { path: normalize_virtual(vp)?, deleted: now() })
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Reserve a fresh id by creating its record.
    let id = loop {
        let id = format!("{}-{}-{}", now(), std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed));
        match info.create_file(&record(&id), 0o666) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(&meta) {
                    let _ = info.unlink(&record(&id));
                    return Err(e.into());
                }
                break id;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    };
    if let Err(e) = dir.rename_noreplace(&name, files, OsStr::new(&id)) {
        let _ = info.unlink(&record(&id));
        if crate::confined::errno(&e) == Some(rustix::io::Errno::XDEV) {
            return Err(ApiError::bad_request(
                "It is on a different volume than the Trash; use Delete Permanently (Shift+Del)",
            ));
        }
        return Err(e.into());
    }
    Ok(id)
}

/// Everything in the Trash, most recently deleted first.
pub fn list(root: &Root) -> ApiResult<Vec<TrashItem>> {
    let Some((files, info)) = dirs(root, false)? else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for id in ids(&info) {
        let Ok(meta) = read_meta(&info, &id) else { continue };
        let name = vname(&meta.path).to_string();
        match entry_nofollow(&files, OsStr::new(&id), &name, meta.path) {
            Some(entry) => out.push(TrashItem { id, entry, deleted: meta.deleted }),
            // The item itself is gone: drop the stale record, unless the
            // item may still be on its way in.
            None => {
                let rec = record(&id);
                if info.lstat(&rec).is_ok_and(|m| now() - m.modified > RECORD_GRACE_SECS) {
                    let _ = info.unlink(&rec);
                }
            }
        }
    }
    out.sort_by_key(|t| std::cmp::Reverse(t.deleted));
    Ok(out)
}

/// Put items back where they were. Missing folders are recreated, and a
/// name that has been taken meanwhile gets a " (1)" suffix. `ids` in the
/// report lists the restored paths.
pub fn restore(root: &Root, ids: &[String]) -> OpReport {
    let mut report = OpReport::default();
    let trash = dirs(root, false);
    for id in ids {
        let mut name = id.clone();
        let res = (|| -> ApiResult<String> {
            check_id(id)?;
            let Some((files, info)) = trash.as_ref().map_err(|e| ApiError::internal(e.to_string()))? else {
                return Err(ApiError::not_found("No such item in the Trash"));
            };
            let meta = read_meta(info, id)?;
            // The record names the item; fall back to the id for a broken one.
            name = check_name(vname(&meta.path)).unwrap_or_else(|_| id.clone());
            files.lstat(OsStr::new(id))?;
            let vdir = vparent(&meta.path);
            let dir = fsops::make_dirs(root, "/", &components(&vdir)?)?;
            let target = rename_unique(files, OsStr::new(id), &dir, OsStr::new(&name))?;
            let _ = info.unlink(&record(id));
            Ok(vjoin(&normalize_virtual(&vdir)?, &target.to_string_lossy()))
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

fn purge_one(files: &Dir, info: &Dir, id: &str) -> ApiResult<()> {
    check_id(id)?;
    match files.remove(OsStr::new(id)) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let _ = info.unlink(&record(id));
    Ok(())
}

/// Delete items from the Trash for good.
pub fn purge(root: &Root, ids: &[String]) -> OpReport {
    let mut report = OpReport::default();
    let (files, info) = match dirs(root, false) {
        Ok(Some(d)) => d,
        // No Trash: nothing to delete.
        Ok(None) => return report,
        Err(e) => {
            report.errors.push(format!("Could not open the Trash: {}", ApiError::from(e).message));
            return report;
        }
    };
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
pub fn empty(root: &Root) -> OpReport {
    let Ok(Some((files, info))) = dirs(root, false) else { return purge(root, &[]) };
    let mut report = purge(root, &ids(&info));
    for (name, _) in files.entries().unwrap_or_default() {
        if let Err(e) = files.remove(&name) {
            report.errors.push(format!("{}: {}", name.to_string_lossy(), ApiError::from(e).message));
        }
    }
    report
}

/// Delete items that have been in the Trash for more than `days` days.
pub fn expire(root: &Root, days: u64) -> OpReport {
    let Ok(Some((_, info))) = dirs(root, false) else { return OpReport::default() };
    let age = i64::try_from(days).unwrap_or(i64::MAX).saturating_mul(86_400);
    let cutoff = now().saturating_sub(age);
    let old: Vec<String> = ids(&info)
        .into_iter()
        .filter(|id| read_meta(&info, id).map(|m| m.deleted < cutoff).unwrap_or(false))
        .collect();
    purge(root, &old)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn setup() -> (tempfile::TempDir, PathBuf, Root) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(path.join("docs/sub")).unwrap();
        fs::write(path.join("docs/a.txt"), b"hello").unwrap();
        fs::write(path.join("docs/sub/b.txt"), b"world!").unwrap();
        let root = Root::open(&path, DIR).unwrap();
        (tmp, path, root)
    }

    #[test]
    fn trash_and_restore() {
        let (_t, path, root) = setup();
        let r = trash(&root, &["/docs/a.txt".into(), "/docs/sub".into()]);
        assert_eq!(r.done, 2, "{:?}", r.errors);
        assert!(!path.join("docs/a.txt").exists());
        assert!(!path.join("docs/sub").exists());
        assert_eq!(count(&root), 2);

        let items = list(&root).unwrap();
        assert_eq!(items.len(), 2);
        let file = items.iter().find(|i| i.entry.name == "a.txt").unwrap();
        assert_eq!(file.entry.path, "/docs/a.txt");
        assert_eq!(file.entry.size, 5);

        // The name was taken meanwhile: restore next to it.
        fs::write(path.join("docs/a.txt"), b"new").unwrap();
        let r = restore(&root, std::slice::from_ref(&file.id));
        assert_eq!(r.ids, vec!["/docs/a (1).txt".to_string()]);
        assert_eq!(fs::read(path.join("docs/a (1).txt")).unwrap(), b"hello");
        assert_eq!(count(&root), 1);
    }

    #[test]
    fn restore_recreates_missing_folders() {
        let (_t, path, root) = setup();
        let r = trash(&root, &["/docs/sub/b.txt".into()]);
        fs::remove_dir_all(path.join("docs")).unwrap();
        let r = restore(&root, &r.ids);
        assert_eq!(r.done, 1, "{:?}", r.errors);
        assert_eq!(fs::read(path.join("docs/sub/b.txt")).unwrap(), b"world!");
    }

    #[test]
    fn purge_empty_and_expire() {
        let (_t, path, root) = setup();
        assert_eq!((count(&root), list(&root).unwrap().len(), empty(&root).done), (0, 0, 0), "no Trash yet");
        let r = trash(&root, &["/docs/a.txt".into()]);
        assert_eq!(purge(&root, &r.ids).done, 1);
        assert_eq!(count(&root), 0);
        assert!(fs::read_dir(path.join(DIR).join("files")).unwrap().next().is_none());

        trash(&root, &["/docs/sub".into()]);
        assert_eq!(expire(&root, 30).done, 0);
        // Huge values mean "keep", not an overflow that empties the Trash.
        assert_eq!(expire(&root, u64::MAX).done, 0);
        assert_eq!(count(&root), 1);
        empty(&root);
        assert_eq!(count(&root), 0);
        assert!(list(&root).unwrap().is_empty());
    }

    #[test]
    fn records_of_items_on_their_way_in_are_kept() {
        let (_t, path, root) = setup();
        let r = trash(&root, &["/docs/a.txt".into()]);
        // As if `list` ran between writing the record and moving the item.
        fs::rename(path.join(DIR).join("files").join(&r.ids[0]), path.join("away")).unwrap();
        assert!(list(&root).unwrap().is_empty());
        assert_eq!(count(&root), 1, "a fresh record survives a listing");
    }

    #[test]
    fn trash_is_unreachable_and_ids_are_checked() {
        let (_t, path, root) = setup();
        trash(&root, &["/docs/a.txt".into()]);
        assert!(fsops::open_dir(&root, &format!("/{DIR}/files")).is_err());
        assert!(fsops::list_dir(&root, "/").unwrap().entries.iter().all(|e| e.name != DIR));
        assert!(fsops::sanitize_name(DIR).is_err());
        assert!(restore(&root, &["../../docs".into()]).errors.len() == 1);
        assert!(purge(&root, &["../docs".into()]).errors.len() == 1);
        assert!(path.join("docs").exists());
        assert!(trash(&root, &["/".into()]).errors.len() == 1);
        // Not through a symlink either.
        std::os::unix::fs::symlink(DIR, path.join("t")).unwrap();
        assert!(fsops::list_dir(&root, "/t").is_err());
    }

    #[test]
    fn a_trash_folder_that_is_a_symlink_is_never_used() {
        let (_t, path, root) = setup();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(outside.path().join("files/victim")).unwrap();
        fs::create_dir(outside.path().join("info")).unwrap();
        std::os::unix::fs::symlink(outside.path(), path.join(DIR)).unwrap();
        empty(&root);
        expire(&root, 0);
        assert!(outside.path().join("files/victim").exists(), "nothing outside the root may be deleted");
        assert_eq!(trash(&root, &["/docs/a.txt".into()]).errors.len(), 1);
        assert!(path.join("docs/a.txt").exists());
    }
}
