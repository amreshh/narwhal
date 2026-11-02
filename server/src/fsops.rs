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
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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

/// Rename `src` to `dst`, failing with `AlreadyExists` instead of replacing
/// an existing `dst`. Atomic where the filesystem supports `RENAME_NOREPLACE`.
pub fn rename_noreplace(src: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let s = CString::new(src.as_os_str().as_bytes()).map_err(|_| io::ErrorKind::InvalidInput)?;
        let d = CString::new(dst.as_os_str().as_bytes()).map_err(|_| io::ErrorKind::InvalidInput)?;
        // SAFETY: both arguments are valid NUL-terminated paths.
        let rc = unsafe {
            libc::renameat2(libc::AT_FDCWD, s.as_ptr(), libc::AT_FDCWD, d.as_ptr(), libc::RENAME_NOREPLACE)
        };
        if rc == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        // Filesystems without RENAME_NOREPLACE support fall through to check-then-rename.
        if !matches!(err.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP)) {
            return Err(err);
        }
    }
    if fs::symlink_metadata(dst).is_ok() {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    fs::rename(src, dst)
}

/// Move `src` into `dir` as `name`, or `name (1)`… if taken. Never overwrites.
pub fn rename_unique(src: &Path, dir: &Path, name: &str) -> io::Result<PathBuf> {
    for _ in 0..100 {
        let target = unique_path(dir, name);
        match rename_noreplace(src, &target) {
            Ok(()) => return Ok(target),
            // Someone took the name in the meantime: pick the next one.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "Could not find a free name"))
}

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Create a fresh hidden temp file in `dir` for an upload. `create_new`
/// refuses anything already at that path, including a planted symlink, so the
/// data can never be written somewhere else.
pub fn create_temp(dir: &Path) -> io::Result<(fs::File, PathBuf)> {
    for _ in 0..100 {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(".narwhal-upload-{}-{seq}.part", std::process::id()));
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => return Ok((f, path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "Could not create a temporary file"))
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
    let src_meta = fs::symlink_metadata(&src)?;
    if req.new_name.contains(['/', '\\']) {
        return Err(ApiError::bad_request("Name may not contain slashes"));
    }
    let name = sanitize_name(&req.new_name)?;
    let parent = src.parent().ok_or_else(|| ApiError::bad_request("Invalid path"))?;
    let target = parent.join(&name);
    if target == src {
        return normalize_virtual(&req.path);
    }
    let exists = || ApiError::conflict(format!("\"{name}\" already exists"));
    match fs::symlink_metadata(&target) {
        // Same file under another spelling: a case-only rename on a
        // case-insensitive filesystem.
        Ok(t) if t.dev() == src_meta.dev() && t.ino() == src_meta.ino() => fs::rename(&src, &target)?,
        Ok(_) => return Err(exists()),
        Err(_) => rename_noreplace(&src, &target).map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                exists()
            } else {
                e.into()
            }
        })?,
    }
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

/// Copy one regular file into a new `dst` (never through an existing path or link).
fn copy_file(src: &Path, dst: &Path, mode: u32) -> io::Result<()> {
    let mut from = fs::File::open(src)?;
    let mut to = fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(dst)?;
    io::copy(&mut from, &mut to)?;
    // The create mode is reduced by the umask; apply the source's exact bits.
    to.set_permissions(fs::Permissions::from_mode(mode))
}

/// Copy a file, symlink or folder tree to `dst`, which must not exist yet.
/// Iterative, so deep trees cannot overflow the stack. On failure, anything
/// already created at `dst` is removed again.
pub fn copy_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    let mut created = false;
    let res = copy_tree(src, dst, &mut created);
    if res.is_err() && created {
        let _ = remove_any(dst);
    }
    res
}

fn copy_tree(src: &Path, dst: &Path, created: &mut bool) -> io::Result<()> {
    let mut stack = vec![(src.to_path_buf(), dst.to_path_buf())];
    // Folder permissions are applied last, so read-only folders can be filled first.
    let mut dirs = Vec::new();
    while let Some((s, d)) = stack.pop() {
        let meta = fs::symlink_metadata(&s)?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&s)?, &d)?;
        } else if ft.is_dir() {
            fs::create_dir(&d)?;
            *created = true;
            for de in fs::read_dir(&s)? {
                let de = de?;
                stack.push((de.path(), d.join(de.file_name())));
            }
            dirs.push((d, meta.permissions()));
            continue;
        } else if ft.is_file() {
            copy_file(&s, &d, meta.permissions().mode() & 0o7777)?;
        } else {
            // FIFOs, sockets and devices: opening a FIFO would block forever.
            let name = s.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("\"{name}\" is a special file and cannot be copied"),
            ));
        }
        *created = true;
    }
    for (d, perms) in dirs.into_iter().rev() {
        fs::set_permissions(d, perms)?;
    }
    Ok(())
}

fn transfer_one(root: &Path, vp: &str, dest: &Path, op: TransferOp) -> ApiResult<()> {
    let src = resolve(root, vp)?;
    if src == root {
        return Err(ApiError::forbidden("The root folder cannot be moved"));
    }
    fs::symlink_metadata(&src)?;
    let name = src
        .file_name()
        .ok_or_else(|| ApiError::bad_request("Invalid path"))?
        .to_string_lossy()
        .into_owned();
    // Compare real locations, so a symlinked destination cannot point back
    // into the source. The source's own last component is not followed: a
    // symlink is copied or moved as a link.
    let src_parent = src
        .parent()
        .ok_or_else(|| ApiError::bad_request("Invalid path"))?
        .canonicalize()?;
    let src_real = src_parent.join(&name);
    let dest_real = dest.canonicalize()?;
    if dest_real.starts_with(&src_real) {
        return Err(ApiError::bad_request("A folder cannot be copied or moved into itself"));
    }
    if op == TransferOp::Move && src_parent == dest_real {
        return Ok(()); // Moving onto itself: nothing to do.
    }
    match op {
        TransferOp::Copy => copy_recursive(&src, &unique_path(dest, &name))?,
        TransferOp::Move => match rename_unique(&src, dest, &name) {
            Ok(_) => {}
            // Different filesystems (e.g. two volumes): copy then delete.
            // copy_recursive cleans up a partial copy if it fails.
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                copy_recursive(&src, &unique_path(dest, &name))?;
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
    fn case_only_rename_does_not_clobber_another_file() {
        let (_t, root) = setup();
        fs::write(root.join("Foo"), b"one").unwrap();
        fs::write(root.join("foo"), b"two").unwrap();
        let r = rename(&root, &RenameReq { path: "/Foo".into(), new_name: "foo".into() });
        assert_eq!(r.unwrap_err().status, axum::http::StatusCode::CONFLICT);
        assert_eq!(fs::read(root.join("Foo")).unwrap(), b"one");
        assert_eq!(fs::read(root.join("foo")).unwrap(), b"two");
    }

    #[test]
    fn case_only_rename_works_when_free() {
        let (_t, root) = setup();
        fs::write(root.join("Foo"), b"one").unwrap();
        let p = rename(&root, &RenameReq { path: "/Foo".into(), new_name: "foo".into() }).unwrap();
        assert_eq!(p, "/foo");
        assert_eq!(fs::read(root.join("foo")).unwrap(), b"one");
    }

    #[test]
    fn rename_noreplace_refuses_existing_target() {
        let (_t, root) = setup();
        let err = rename_noreplace(&root.join("docs/a.txt"), &root.join("docs/sub/b.txt")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(root.join("docs/sub/b.txt")).unwrap(), b"world!");
        assert!(root.join("docs/a.txt").exists());
    }

    #[test]
    fn rename_unique_picks_a_free_name() {
        let (_t, root) = setup();
        fs::write(root.join("up.part"), b"new").unwrap();
        let t = rename_unique(&root.join("up.part"), &root.join("docs"), "a.txt").unwrap();
        assert_eq!(t, root.join("docs/a (1).txt"));
        assert_eq!(fs::read(root.join("docs/a.txt")).unwrap(), b"hello");
    }

    #[test]
    fn copy_into_itself_through_symlink_is_rejected() {
        let (_t, root) = setup();
        std::os::unix::fs::symlink("docs", root.join("link")).unwrap();
        for op in [TransferOp::Copy, TransferOp::Move] {
            let r = transfer(&root, &TransferReq { paths: vec!["/docs".into()], dest: "/link".into(), op })
                .unwrap();
            assert_eq!((r.done, r.errors.len()), (0, 1), "{op:?} into itself via a link must fail");
        }
        let r = transfer(
            &root,
            &TransferReq { paths: vec!["/docs".into()], dest: "/link/sub".into(), op: TransferOp::Copy },
        )
        .unwrap();
        assert_eq!(r.errors.len(), 1);
        assert!(!root.join("docs/docs").exists());
        assert!(!root.join("docs/sub/docs").exists());
    }

    #[test]
    fn moving_a_symlink_moves_the_link_itself() {
        let (_t, root) = setup();
        std::os::unix::fs::symlink("docs", root.join("docs/sub/up")).unwrap();
        // The link points at an ancestor, but moving the link (not its target) is fine.
        let r = transfer(
            &root,
            &TransferReq { paths: vec!["/docs/sub/up".into()], dest: "/".into(), op: TransferOp::Move },
        )
        .unwrap();
        assert_eq!(r.done, 1, "{:?}", r.errors);
        assert!(fs::symlink_metadata(root.join("up")).unwrap().file_type().is_symlink());
    }

    #[test]
    fn copy_preserves_tree_links_and_permissions() {
        let (_t, root) = setup();
        std::os::unix::fs::symlink("a.txt", root.join("docs/ln")).unwrap();
        fs::set_permissions(root.join("docs/a.txt"), fs::Permissions::from_mode(0o640)).unwrap();
        fs::set_permissions(root.join("docs/sub"), fs::Permissions::from_mode(0o555)).unwrap();
        copy_recursive(&root.join("docs"), &root.join("copy")).unwrap();
        assert_eq!(fs::read(root.join("copy/sub/b.txt")).unwrap(), b"world!");
        assert_eq!(fs::read_link(root.join("copy/ln")).unwrap(), Path::new("a.txt"));
        let mode = |p: &str| fs::metadata(root.join(p)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode("copy/a.txt"), 0o640);
        assert_eq!(mode("copy/sub"), 0o555);
        fs::set_permissions(root.join("docs/sub"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(root.join("copy/sub"), fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn copy_handles_deep_trees_without_stack_overflow() {
        let (_t, root) = setup();
        let mut deep = root.join("deep");
        for _ in 0..1500 {
            deep.push("d");
        }
        fs::create_dir_all(&deep).unwrap();
        copy_recursive(&root.join("deep"), &root.join("deep2")).unwrap();
        assert!(root.join("deep2").join(deep.strip_prefix(root.join("deep")).unwrap()).is_dir());
    }

    #[test]
    fn failed_copy_cleans_up_and_never_overwrites() {
        let (_t, root) = setup();
        let fifo = std::ffi::CString::new(root.join("docs/sub/pipe").into_os_string().into_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
        assert!(copy_recursive(&root.join("docs"), &root.join("copy")).is_err());
        assert!(!root.join("copy").exists(), "partial copy must be removed");
        // An existing destination is refused and left alone.
        assert!(copy_recursive(&root.join("docs/a.txt"), &root.join("docs/sub/b.txt")).is_err());
        assert_eq!(fs::read(root.join("docs/sub/b.txt")).unwrap(), b"world!");
    }

    #[test]
    fn upload_temp_never_follows_a_planted_symlink() {
        let (_t, root) = setup();
        let outside = tempfile::tempdir().unwrap();
        // Plant dangling links outside the root under the next temp names.
        let next = TEMP_SEQ.load(Ordering::Relaxed);
        for seq in next..next + 50 {
            let name = format!(".narwhal-upload-{}-{seq}.part", std::process::id());
            std::os::unix::fs::symlink(outside.path().join(&name), root.join("docs").join(&name)).unwrap();
        }
        let (_f, tmp) = create_temp(&root.join("docs")).unwrap();
        assert!(fs::symlink_metadata(&tmp).unwrap().file_type().is_file());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0, "nothing may be written outside the root");
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
