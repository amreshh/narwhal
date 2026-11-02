//! Synchronous filesystem operations, confined to a root directory.
//!
//! Every function takes the canonical root and *virtual* paths (see
//! `narwhal_shared`). They are run on Tokio's blocking pool by the handlers.

use crate::error::{ApiError, ApiResult};
use narwhal_shared::{
    vjoin, vname, CreateReq, DiskInfo, Entry, EntryKind, Listing, OpReport, Place, RenameReq,
    SizeReport, TransferOp, TransferReq,
};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Hard cap on entries visited by a recursive search or size walk.
const WALK_LIMIT: usize = 500_000;

/// Split a virtual path into validated components. Rejects `..`.
fn components(virt: &str) -> ApiResult<Vec<String>> {
    let mut out = Vec::new();
    for c in Path::new(virt).components() {
        match c {
            Component::Normal(s) => out.push(s.to_string_lossy().into_owned()),
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) => {
                return Err(ApiError::forbidden("Path may not contain '..'"))
            }
        }
    }
    Ok(out)
}

/// Normalize a virtual path: `a//b/./c/` -> `/a/b/c`.
pub fn normalize_virtual(virt: &str) -> ApiResult<String> {
    Ok(format!("/{}", components(virt)?.join("/")))
}

/// Map a virtual path to a real path under `root`, refusing anything that
/// would escape it (including via symlinks).
pub fn resolve(root: &Path, virt: &str) -> ApiResult<PathBuf> {
    let mut path = root.to_path_buf();
    for c in components(virt)? {
        path.push(c);
    }
    // Canonicalize the deepest existing ancestor and make sure it is inside root.
    let mut probe: &Path = &path;
    loop {
        match probe.canonicalize() {
            Ok(real) => {
                if !real.starts_with(root) {
                    return Err(ApiError::forbidden("Path is outside the storage root"));
                }
                break;
            }
            Err(_) => match probe.parent() {
                Some(parent) => probe = parent,
                None => break,
            },
        }
    }
    Ok(path)
}

/// Name for error messages ("/" for the root).
fn label(vp: &str) -> &str {
    match vname(vp) {
        "" => "/",
        n => n,
    }
}

/// Validate a single file or folder name supplied by the client.
pub fn sanitize_name(raw: &str) -> ApiResult<String> {
    // Browsers may send a full path for uploads on some platforms; keep the basename.
    let name = raw.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if name.is_empty() || name == "." || name == ".." || name.contains('\0') {
        return Err(ApiError::bad_request("Invalid name"));
    }
    if name.len() > 255 {
        return Err(ApiError::bad_request("Name is too long"));
    }
    Ok(name.to_string())
}

/// `dir/name` if free, otherwise `dir/stem (1).ext`, `dir/stem (2).ext`, ...
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if fs::symlink_metadata(&candidate).is_err() {
        return candidate;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| fs::symlink_metadata(p).is_err())
        .expect("infinite iterator")
}

/// Build an `Entry` for a real path. Returns None if it vanished meanwhile.
pub fn entry_for(full: &Path, name: String, vpath: String) -> Option<Entry> {
    let lmeta = fs::symlink_metadata(full).ok()?;
    let symlink = lmeta.file_type().is_symlink();
    let meta = if symlink { fs::metadata(full).ok() } else { Some(lmeta) };
    let (kind, size, modified, mode) = match &meta {
        Some(m) => {
            let kind = if m.is_dir() {
                EntryKind::Dir
            } else if m.is_file() {
                EntryKind::File
            } else {
                EntryKind::Other
            };
            let modified = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64);
            let size = if m.is_file() { m.len() } else { 0 };
            (kind, size, modified, Some(m.permissions().mode() & 0o7777))
        }
        // Broken symlink.
        None => (EntryKind::Other, 0, None, None),
    };
    let children = if kind == EntryKind::Dir {
        fs::read_dir(full).ok().map(|rd| rd.take(100_000).count() as u32)
    } else {
        None
    };
    Some(Entry {
        hidden: name.starts_with('.'),
        name,
        path: vpath,
        kind,
        size,
        children,
        modified,
        symlink,
        mode,
    })
}

pub fn list_dir(root: &Path, virt: &str) -> ApiResult<Listing> {
    let dir = resolve(root, virt)?;
    if !fs::metadata(&dir)?.is_dir() {
        return Err(ApiError::bad_request("Not a folder"));
    }
    let vdir = normalize_virtual(virt)?;
    let mut entries = Vec::new();
    for de in fs::read_dir(&dir)?.flatten() {
        let name = de.file_name().to_string_lossy().into_owned();
        let vpath = vjoin(&vdir, &name);
        if let Some(e) = entry_for(&de.path(), name, vpath) {
            entries.push(e);
        }
    }
    Ok(Listing { path: vdir, entries })
}

fn parent_dir(root: &Path, virt: &str) -> ApiResult<PathBuf> {
    let dir = resolve(root, virt)?;
    if !fs::metadata(&dir)?.is_dir() {
        return Err(ApiError::bad_request("Not a folder"));
    }
    Ok(dir)
}

pub fn create_dir(root: &Path, req: &CreateReq) -> ApiResult<String> {
    let dir = parent_dir(root, &req.path)?;
    let name = sanitize_name(&req.name)?;
    fs::create_dir(dir.join(&name))?;
    Ok(vjoin(&normalize_virtual(&req.path)?, &name))
}

pub fn create_file(root: &Path, req: &CreateReq) -> ApiResult<String> {
    let dir = parent_dir(root, &req.path)?;
    let name = sanitize_name(&req.name)?;
    fs::OpenOptions::new().write(true).create_new(true).open(dir.join(&name))?;
    Ok(vjoin(&normalize_virtual(&req.path)?, &name))
}

pub fn rename(root: &Path, req: &RenameReq) -> ApiResult<String> {
    let src = resolve(root, &req.path)?;
    if src == root {
        return Err(ApiError::forbidden("The root folder cannot be renamed"));
    }
    fs::symlink_metadata(&src)?;
    if req.new_name.contains(['/', '\\']) {
        return Err(ApiError::bad_request("Name may not contain slashes"));
    }
    let name = sanitize_name(&req.new_name)?;
    let parent = src.parent().ok_or_else(|| ApiError::bad_request("Invalid path"))?;
    let target = parent.join(&name);
    if target == src {
        return normalize_virtual(&req.path);
    }
    // Allow case-only renames on case-insensitive filesystems, otherwise refuse to clobber.
    let case_only = target.to_string_lossy().to_lowercase() == src.to_string_lossy().to_lowercase();
    if !case_only && fs::symlink_metadata(&target).is_ok() {
        return Err(ApiError::conflict(format!("\"{name}\" already exists")));
    }
    fs::rename(&src, &target)?;
    let vparent = narwhal_shared::vparent(&normalize_virtual(&req.path)?);
    Ok(vjoin(&vparent, &name))
}

fn remove_any(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

pub fn delete(root: &Path, paths: &[String]) -> OpReport {
    let mut report = OpReport::default();
    for vp in paths {
        let result = resolve(root, vp).and_then(|p| {
            if p == root {
                return Err(ApiError::forbidden("The root folder cannot be deleted"));
            }
            remove_any(&p).map_err(ApiError::from)
        });
        match result {
            Ok(()) => report.done += 1,
            Err(e) => report.errors.push(format!("{}: {}", label(vp), e.message)),
        }
    }
    report
}

pub fn copy_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(src)?, dst)
    } else if ft.is_dir() {
        fs::create_dir(dst)?;
        for de in fs::read_dir(src)? {
            let de = de?;
            copy_recursive(&de.path(), &dst.join(de.file_name()))?;
        }
        fs::set_permissions(dst, meta.permissions())
    } else {
        fs::copy(src, dst).map(|_| ())
    }
}

fn transfer_one(root: &Path, vp: &str, dest: &Path, op: TransferOp) -> ApiResult<()> {
    let src = resolve(root, vp)?;
    if src == root {
        return Err(ApiError::forbidden("The root folder cannot be moved"));
    }
    fs::symlink_metadata(&src)?;
    if dest.starts_with(&src) {
        return Err(ApiError::bad_request("A folder cannot be copied or moved into itself"));
    }
    let name = src
        .file_name()
        .ok_or_else(|| ApiError::bad_request("Invalid path"))?
        .to_string_lossy()
        .into_owned();
    if op == TransferOp::Move && src.parent() == Some(dest) {
        return Ok(()); // Moving onto itself: nothing to do.
    }
    let target = unique_path(dest, &name);
    match op {
        TransferOp::Copy => {
            if let Err(e) = copy_recursive(&src, &target) {
                let _ = remove_any(&target);
                return Err(e.into());
            }
        }
        TransferOp::Move => match fs::rename(&src, &target) {
            Ok(()) => {}
            // Different filesystems (e.g. two volumes): copy then delete.
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                copy_recursive(&src, &target)?;
                remove_any(&src)?;
            }
            Err(e) => return Err(e.into()),
        },
    }
    Ok(())
}

pub fn transfer(root: &Path, req: &TransferReq) -> ApiResult<OpReport> {
    let dest = resolve(root, &req.dest)?;
    if !fs::metadata(&dest)?.is_dir() {
        return Err(ApiError::bad_request("Destination is not a folder"));
    }
    let mut report = OpReport::default();
    for vp in &req.paths {
        match transfer_one(root, vp, &dest, req.op) {
            Ok(()) => report.done += 1,
            Err(e) => report.errors.push(format!("{}: {}", label(vp), e.message)),
        }
    }
    Ok(report)
}

/// Case-insensitive recursive name search below `virt`. Does not follow
/// symlinked folders (avoids loops).
pub fn search(root: &Path, virt: &str, query: &str, limit: usize) -> ApiResult<Vec<Entry>> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let start = resolve(root, virt)?;
    let mut stack = vec![(start, normalize_virtual(virt)?)];
    let mut out = Vec::new();
    let mut visited = 0usize;
    while let Some((dir, vdir)) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for de in rd.flatten() {
            visited += 1;
            if visited > WALK_LIMIT {
                return Ok(out);
            }
            let name = de.file_name().to_string_lossy().into_owned();
            let vpath = vjoin(&vdir, &name);
            let is_real_dir = de.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if name.to_lowercase().contains(&needle) {
                if let Some(e) = entry_for(&de.path(), name, vpath.clone()) {
                    out.push(e);
                    if out.len() >= limit {
                        return Ok(out);
                    }
                }
            }
            if is_real_dir {
                stack.push((de.path(), vpath));
            }
        }
    }
    Ok(out)
}

/// Total size of a file or folder tree (symlinks are not followed).
pub fn size_of(root: &Path, virt: &str) -> ApiResult<SizeReport> {
    let start = resolve(root, virt)?;
    let mut report = SizeReport::default();
    let meta = fs::symlink_metadata(&start)?;
    if !meta.is_dir() {
        report.bytes = meta.len();
        report.files = 1;
        return Ok(report);
    }
    let mut stack = vec![start];
    let mut visited = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for de in rd.flatten() {
            visited += 1;
            if visited > WALK_LIMIT {
                return Ok(report);
            }
            let Ok(m) = de.metadata() else { continue }; // DirEntry::metadata does not follow symlinks
            if m.is_dir() {
                report.dirs += 1;
                stack.push(de.path());
            } else {
                report.files += 1;
                report.bytes += m.len();
            }
        }
    }
    Ok(report)
}

pub fn disk_info(path: &Path) -> Option<DiskInfo> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs is plain-old-data; we pass a valid NUL-terminated path.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let frsize = s.f_frsize as u64;
    Some(DiskInfo {
        total: s.f_blocks as u64 * frsize,
        available: s.f_bavail as u64 * frsize,
    })
}

/// Sidebar places: configured ones if any, otherwise the root plus its
/// top-level (non-hidden) folders.
pub fn places(root: &Path, root_name: &str, configured: &[Place]) -> Vec<Place> {
    let mut out = vec![Place { name: root_name.to_string(), path: "/".into() }];
    if !configured.is_empty() {
        out.extend(
            configured
                .iter()
                .filter(|p| resolve(root, &p.path).map(|r| r.is_dir()).unwrap_or(false))
                .cloned(),
        );
        return out;
    }
    let mut dirs: Vec<String> = fs::read_dir(root)
        .map(|rd| {
            rd.flatten()
                .filter(|de| de.path().is_dir())
                .map(|de| de.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.') && n != "lost+found")
                .collect()
        })
        .unwrap_or_default();
    dirs.sort_by_key(|d| d.to_lowercase());
    out.extend(dirs.into_iter().take(24).map(|n| Place { path: format!("/{n}"), name: n }));
    out
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
    fn traversal_is_rejected() {
        let (_t, root) = setup();
        assert!(resolve(&root, "/../etc/passwd").is_err());
        assert!(resolve(&root, "docs/../../x").is_err());
        assert!(resolve(&root, "/docs/a.txt").is_ok());
    }

    #[test]
    fn symlink_escape_is_rejected() {
        let (_t, root) = setup();
        std::os::unix::fs::symlink("/etc", root.join("evil")).unwrap();
        assert!(resolve(&root, "/evil/passwd").is_err());
        assert!(list_dir(&root, "/evil").is_err());
    }

    #[test]
    fn list_and_sizes() {
        let (_t, root) = setup();
        let l = list_dir(&root, "/docs").unwrap();
        assert_eq!(l.path, "/docs");
        assert_eq!(l.entries.len(), 2);
        let s = size_of(&root, "/docs").unwrap();
        assert_eq!(s.bytes, 11);
        assert_eq!(s.files, 2);
        assert_eq!(s.dirs, 1);
    }

    #[test]
    fn copy_move_rename_delete() {
        let (_t, root) = setup();
        let r = transfer(
            &root,
            &TransferReq { paths: vec!["/docs/a.txt".into()], dest: "/docs".into(), op: TransferOp::Copy },
        )
        .unwrap();
        assert_eq!(r.done, 1);
        assert!(root.join("docs/a (1).txt").exists());

        let r = transfer(
            &root,
            &TransferReq { paths: vec!["/docs".into()], dest: "/docs/sub".into(), op: TransferOp::Move },
        )
        .unwrap();
        assert_eq!(r.errors.len(), 1, "moving a folder into itself must fail");

        rename(&root, &RenameReq { path: "/docs/sub".into(), new_name: "renamed".into() }).unwrap();
        assert!(root.join("docs/renamed/b.txt").exists());
        assert!(rename(&root, &RenameReq { path: "/docs/renamed".into(), new_name: "a.txt".into() }).is_err());

        let r = delete(&root, &["/docs/renamed".into(), "/".into()]);
        assert_eq!(r.done, 1);
        assert_eq!(r.errors.len(), 1, "root must not be deletable");
        assert!(!root.join("docs/renamed").exists());
    }

    #[test]
    fn search_finds_nested() {
        let (_t, root) = setup();
        let hits = search(&root, "/", "B.TXT", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "/docs/sub/b.txt");
    }

    #[test]
    fn unique_names() {
        let (_t, root) = setup();
        assert_eq!(unique_path(&root.join("docs"), "a.txt"), root.join("docs/a (1).txt"));
        assert_eq!(unique_path(&root.join("docs"), "new.txt"), root.join("docs/new.txt"));
        assert!(sanitize_name("..").is_err());
        assert_eq!(sanitize_name("C:\\x\\y.png").unwrap(), "y.png");
    }
}
