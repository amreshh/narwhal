//! Synchronous filesystem operations, confined to the storage root.
//!
//! Every function takes the `Root` and *virtual* paths (see
//! `narwhal_shared`). They are run on Tokio's blocking pool by the handlers.
//! All access goes through `confined`, so no path leads outside the root,
//! even when folders are swapped for symlinks while a request runs.

use crate::confined::{self, Dir, Found, Meta, Root};
use crate::error::{ApiError, ApiResult};
use narwhal_shared::{
    vjoin, vname, CreateReq, DiskInfo, Entry, EntryKind, Listing, OpReport, Place, RenameReq, SizeReport, TransferOp,
    TransferReq,
};
use rustix::fs::FileType;
use rustix::io::Errno;
use std::ffi::{OsStr, OsString};
use std::fs::{File, Permissions};
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Hard cap on entries visited by a recursive search or size walk.
const WALK_LIMIT: usize = 500_000;

/// Seconds since the Unix epoch.
pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Split a virtual path into validated components. Rejects `..`.
pub(crate) fn components(virt: &str) -> ApiResult<Vec<String>> {
    let mut out = Vec::new();
    for c in Path::new(virt).components() {
        match c {
            // The Trash folder is only reachable through the Trash API.
            Component::Normal(s) if out.is_empty() && s == crate::trash::DIR => {
                return Err(ApiError::not_found("No such file or folder"))
            }
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

/// The folder `virt` leads to. Symlinks are followed while they stay
/// inside the root.
pub fn open_dir(root: &Root, virt: &str) -> ApiResult<Dir> {
    Ok(root.open_dir(&components(virt)?)?)
}

/// The folder that holds `virt`, and its name there: for working on the
/// entry itself, which is never followed if it is a symlink. None for the
/// root.
pub fn locate(root: &Root, virt: &str) -> ApiResult<Option<(Dir, OsString)>> {
    let mut names = components(virt)?;
    let Some(name) = names.pop() else { return Ok(None) };
    Ok(Some((root.open_dir(&names)?, OsString::from(name))))
}

/// The file `virt` leads to, opened only to name it, with its metadata.
pub fn open_file(root: &Root, virt: &str) -> ApiResult<(OwnedFd, Meta)> {
    let not_a_file = || ApiError::bad_request("Only files can be downloaded");
    match root.find(&components(virt)?)? {
        Found::Dir(_) => Err(not_a_file()),
        Found::Other(fd) => {
            let meta = confined::fstat(&fd)?;
            if meta.is_file() {
                Ok((fd, meta))
            } else {
                Err(not_a_file())
            }
        }
    }
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
    check_name(raw.rsplit(['/', '\\']).next().unwrap_or(""))
}

pub(crate) fn check_name(raw: &str) -> ApiResult<String> {
    let name = raw.trim();
    if name.is_empty() || name == "." || name == ".." || name.contains(['\0', '/', '\\']) {
        return Err(ApiError::bad_request("Invalid name"));
    }
    if name == crate::trash::DIR {
        return Err(ApiError::bad_request("That name is reserved for the Trash"));
    }
    if name.len() > 255 {
        return Err(ApiError::bad_request("Name is too long"));
    }
    Ok(name.to_string())
}

/// Split the relative path of an uploaded file (`photos/2024/a.jpg`, sent
/// for folder uploads) into its folders and its file name. Every component
/// is validated, so `..` and the like are refused.
pub fn split_upload_path(rel: &str) -> ApiResult<(Vec<String>, String)> {
    let mut parts = rel.split('/').filter(|p| !p.is_empty()).map(check_name).collect::<ApiResult<Vec<_>>>()?;
    let name = parts.pop().ok_or_else(|| ApiError::bad_request("Invalid name"))?;
    Ok((parts, name))
}

/// Open the folder `vbase`, create the folders `dirs` below it as needed,
/// and return the deepest one. Existing folders are reused, including
/// symlinks to folders inside the root.
pub fn make_dirs(root: &Root, vbase: &str, dirs: &[String]) -> ApiResult<Dir> {
    let mut names = components(vbase)?;
    let mut dir = root.open_dir(&names)?;
    for d in dirs {
        names.push(d.clone());
        let name = OsStr::new(d);
        dir = match dir.mkdir(name, 0o777) {
            Ok(()) => dir.open_dir(name)?,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => match dir.open_dir(name) {
                Ok(sub) => sub,
                // A symlink (followed, inside the root only) or a file.
                Err(_) => match root.open_dir(&names) {
                    Ok(sub) => sub,
                    Err(e) if e.kind() == io::ErrorKind::NotADirectory => {
                        return Err(ApiError::conflict(format!("'{d}' already exists and is not a folder")))
                    }
                    Err(e) => return Err(e.into()),
                },
            },
            Err(e) => return Err(e.into()),
        };
    }
    Ok(dir)
}

/// `name`, then `stem (1).ext`, `stem (2).ext`, ... (endless).
fn name_candidates(name: &OsStr) -> impl Iterator<Item = OsString> + '_ {
    let bytes = name.as_bytes();
    let (stem, ext) = match bytes.iter().rposition(|&b| b == b'.') {
        Some(i) if i > 0 => bytes.split_at(i),
        _ => (bytes, &b""[..]),
    };
    std::iter::once(name.to_owned()).chain((1u64..).map(move |n| {
        let mut v = stem.to_vec();
        v.extend_from_slice(format!(" ({n})").as_bytes());
        v.extend_from_slice(ext);
        OsString::from_vec(v)
    }))
}

/// `name` if it is free in `dir`, otherwise `stem (1).ext`, `stem (2).ext`, ...
pub fn unique_name(dir: &Dir, name: &OsStr) -> OsString {
    name_candidates(name).find(|n| dir.lstat(n).is_err()).expect("infinite iterator")
}

/// Lower-case hex SHA-256 of a digest's bytes.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn file_sha256(mut file: File) -> io::Result<String> {
    use io::Read;
    use sha2::{Digest, Sha256};
    // sha2 0.11 hashers no longer implement io::Write, so no io::copy.
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 64 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(hex(&hasher.finalize()))
}

/// A file in `dir` called `name` or `name (n)` with exactly this content.
/// When a retried upload's first attempt did arrive (only the answer was
/// lost), this finds it, so the retry doesn't create a duplicate. Stops at
/// the first free name, as an earlier attempt can't be beyond it.
pub fn find_identical(dir: &Dir, name: &OsStr, size: u64, sha256: &str) -> Option<OsString> {
    for candidate in name_candidates(name).take(1000) {
        let meta = dir.lstat(&candidate).ok()?;
        if meta.is_file() && meta.len == size && file_sha256(dir.open_file(&candidate).ok()?).ok()? == sha256 {
            return Some(candidate);
        }
    }
    None
}

/// Move `name` from `from` into `to` as `to_name`, or `to_name (1)`… if
/// taken. Never overwrites. Returns the name it got.
pub fn rename_unique(from: &Dir, name: &OsStr, to: &Dir, to_name: &OsStr) -> io::Result<OsString> {
    for _ in 0..100 {
        let target = unique_name(to, to_name);
        match from.rename_noreplace(name, to, &target) {
            Ok(()) => return Ok(target),
            // Someone took the name in the meantime: pick the next one.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "Could not find a free name"))
}

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

const TEMP_PREFIX: &str = ".narwhal-upload-";

/// An upload's temp file. Hidden from listings and searches.
fn is_upload_temp(name: &str) -> bool {
    name.starts_with(TEMP_PREFIX) && name.ends_with(".part")
}

/// Temp files of uploads that were cut off by a server restart are left
/// behind (a live upload's temp file is written to all the time). Remove
/// them once they are a day old, whenever their folder is listed.
fn remove_if_stale(dir: &Dir, name: &OsStr) {
    if dir.lstat(name).is_ok_and(|m| now() - m.modified > 86_400) {
        let _ = dir.unlink(name);
    }
}

/// Create a fresh hidden temp file in `dir` for an upload and return it with
/// its name. It is never created through an existing path or a planted
/// symlink, so the data can never be written somewhere else.
pub fn create_temp(dir: &Dir) -> io::Result<(File, OsString)> {
    for _ in 0..100 {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let name = OsString::from(format!("{TEMP_PREFIX}{}-{seq}.part", std::process::id()));
        match dir.create_file(&name, 0o666) {
            Ok(f) => return Ok((f, name)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "Could not create a temporary file"))
}

/// Build an `Entry` for `name` in `dir` (whose virtual path is `vpath`).
/// A symlink is described by its target, looked up from the root, so only
/// targets inside the root are described at all. Returns None if the entry
/// vanished meanwhile.
pub fn entry_for(root: &Root, dir: &Dir, name: &OsStr, vpath: String) -> Option<Entry> {
    let lmeta = dir.lstat(name).ok()?;
    let target = if lmeta.is_symlink() {
        // Broken, or leading out of the root: not described.
        components(&vpath).ok().and_then(|names| match root.find(&names).ok()? {
            Found::Dir(d) => Some((d.stat().ok()?, Some(d))),
            Found::Other(fd) => Some((confined::fstat(&fd).ok()?, None)),
        })
    } else if lmeta.is_dir() {
        Some((lmeta, dir.open_dir(name).ok()))
    } else {
        Some((lmeta, None))
    };
    Some(describe(name, vpath, lmeta.is_symlink(), target))
}

/// An `Entry` for `name` itself, never following a symlink (for the Trash,
/// where a link's target means nothing).
pub fn entry_nofollow(dir: &Dir, name: &OsStr, shown: &str, vpath: String) -> Option<Entry> {
    let meta = dir.lstat(name).ok()?;
    let sub = meta.is_dir().then(|| dir.open_dir(name).ok()).flatten();
    let mut e = describe(OsStr::new(shown), vpath, meta.is_symlink(), Some((meta, sub)));
    if e.symlink {
        // Its target is relative to where it was; described as a link only.
        (e.kind, e.size) = (EntryKind::Other, 0);
    }
    Some(e)
}

fn describe(name: &OsStr, vpath: String, symlink: bool, target: Option<(Meta, Option<Dir>)>) -> Entry {
    let (kind, size, modified, mode, children) = match target {
        Some((m, dir)) => {
            let kind = if m.is_dir() {
                EntryKind::Dir
            } else if m.is_file() {
                EntryKind::File
            } else {
                EntryKind::Other
            };
            let children = dir.filter(|_| m.is_dir()).and_then(|d| d.count_entries(100_000).ok());
            (kind, if m.is_file() { m.len } else { 0 }, Some(m.modified), Some(m.mode), children)
        }
        None => (EntryKind::Other, 0, None, None, None),
    };
    let name = name.to_string_lossy().into_owned();
    Entry {
        hidden: name.starts_with('.'),
        name,
        path: vpath,
        kind,
        size,
        children: children.map(|n| n as u32),
        modified,
        symlink,
        mode,
    }
}

pub fn list_dir(root: &Root, virt: &str) -> ApiResult<Listing> {
    let dir = open_dir(root, virt)?;
    let vdir = normalize_virtual(virt)?;
    let in_root = root.is_root(&dir);
    let mut entries = Vec::new();
    for (name, _) in dir.entries()? {
        let shown = name.to_string_lossy();
        if in_root && shown == crate::trash::DIR {
            continue;
        }
        if is_upload_temp(&shown) {
            remove_if_stale(&dir, &name);
            continue;
        }
        let vpath = vjoin(&vdir, &shown);
        if let Some(e) = entry_for(root, &dir, &name, vpath) {
            entries.push(e);
        }
    }
    Ok(Listing { path: vdir, entries })
}

pub fn create_dir(root: &Root, req: &CreateReq) -> ApiResult<String> {
    let dir = open_dir(root, &req.path)?;
    let name = sanitize_name(&req.name)?;
    dir.mkdir(OsStr::new(&name), 0o777)?;
    Ok(vjoin(&normalize_virtual(&req.path)?, &name))
}

pub fn create_file(root: &Root, req: &CreateReq) -> ApiResult<String> {
    let dir = open_dir(root, &req.path)?;
    let name = sanitize_name(&req.name)?;
    dir.create_file(OsStr::new(&name), 0o666)?;
    Ok(vjoin(&normalize_virtual(&req.path)?, &name))
}

pub fn rename(root: &Root, req: &RenameReq) -> ApiResult<String> {
    let Some((dir, old)) = locate(root, &req.path)? else {
        return Err(ApiError::forbidden("The root folder cannot be renamed"));
    };
    let src_meta = dir.lstat(&old)?;
    if req.new_name.contains(['/', '\\']) {
        return Err(ApiError::bad_request("Name may not contain slashes"));
    }
    let name = sanitize_name(&req.new_name)?;
    let new = OsStr::new(&name);
    if new == old {
        return normalize_virtual(&req.path);
    }
    let exists = || ApiError::conflict(format!("\"{name}\" already exists"));
    match dir.lstat(new) {
        // Same file under another spelling: a case-only rename on a
        // case-insensitive filesystem.
        Ok(t) if t.same_file(&src_meta) => dir.rename(&old, &dir, new)?,
        Ok(_) => return Err(exists()),
        Err(_) => dir.rename_noreplace(&old, &dir, new).map_err(|e| {
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

pub fn delete(root: &Root, paths: &[String]) -> OpReport {
    let mut report = OpReport::default();
    for vp in paths {
        let result = locate(root, vp).and_then(|loc| match loc {
            None => Err(ApiError::forbidden("The root folder cannot be deleted")),
            Some((dir, name)) => dir.remove(&name).map_err(ApiError::from),
        });
        match result {
            Ok(()) => report.done += 1,
            Err(e) => report.errors.push(format!("{}: {}", label(vp), e.message)),
        }
    }
    report
}

/// Copy `name` in `from` (a file, symlink or folder tree) to `to_name` in
/// `to`, which must not exist yet. Symlinks are copied as links. On
/// failure, anything already created is removed again.
pub fn copy(from: &Dir, name: &OsStr, to: &Dir, to_name: &OsStr) -> io::Result<()> {
    let mut created = false;
    let res = copy_tree(from, name, to, to_name, &mut created);
    if res.is_err() && created {
        let _ = to.remove(to_name);
    }
    res
}

/// Copy one entry that is not a folder.
fn copy_entry(from: &Dir, name: &OsStr, meta: &Meta, to: &Dir, to_name: &OsStr) -> io::Result<()> {
    match meta.kind {
        FileType::Symlink => to.symlink(&from.read_link(name)?, to_name),
        FileType::RegularFile => {
            let mut src = from.open_file(name)?;
            let mut dst = to.create_file(to_name, 0o600)?;
            io::copy(&mut src, &mut dst)?;
            // The source's permissions, but never setuid/setgid on a copy.
            dst.set_permissions(Permissions::from_mode(meta.mode & 0o777))
        }
        // FIFOs, sockets and devices: opening a FIFO would block forever.
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("\"{}\" is a special file and cannot be copied", name.to_string_lossy()),
        )),
    }
}

/// Owner can still add to and enter the folder with this mode, so it can
/// be applied before the folder is filled.
fn fillable(mode: u32) -> bool {
    mode & 0o300 == 0o300
}

/// Depth first and without recursion, keeping one source and one target
/// folder per level open.
fn copy_tree(from: &Dir, name: &OsStr, to: &Dir, to_name: &OsStr, created: &mut bool) -> io::Result<()> {
    let meta = from.lstat(name)?;
    if !meta.is_dir() {
        copy_entry(from, name, &meta, to, to_name)?;
        *created = true;
        return Ok(());
    }
    struct Level {
        src: Dir,
        dst: Dir,
        mode: u32,
        /// Below the top folder, for folders whose mode is applied last.
        path: Vec<OsString>,
        todo: Vec<OsString>,
    }
    let level = |src: Dir, dst: Dir, mode: u32, path: Vec<OsString>| -> io::Result<Level> {
        let todo = src.entries()?.into_iter().map(|(n, _)| n).collect();
        Ok(Level { src, dst, mode, path, todo })
    };
    // New folders stay private until they get the source's mode.
    to.mkdir(to_name, 0o700)?;
    *created = true;
    let mut levels = vec![level(from.open_dir(name)?, to.open_dir(to_name)?, meta.mode, Vec::new())?];
    // Modes that would keep the owner out (e.g. read-only folders) are
    // applied once everything is copied, deepest first.
    let mut restrictive: Vec<(Vec<OsString>, u32)> = Vec::new();
    while let Some(top) = levels.last_mut() {
        let Some(child) = top.todo.pop() else {
            let done = levels.pop().expect("a level is open");
            if fillable(done.mode) {
                done.dst.set_mode(done.mode)?;
            } else {
                restrictive.push((done.path, done.mode));
            }
            continue;
        };
        let m = match top.src.lstat(&child) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue, // deleted meanwhile
            Err(e) => return Err(e),
        };
        if !m.is_dir() {
            copy_entry(&top.src, &child, &m, &top.dst, &child)?;
            continue;
        }
        top.dst.mkdir(&child, 0o700)?;
        let (src, dst) = (top.src.open_dir(&child)?, top.dst.open_dir(&child)?);
        let mut path = top.path.clone();
        path.push(child);
        let next = level(src, dst, m.mode, path)?;
        levels.push(next);
    }
    for (path, mode) in restrictive {
        let mut dir = to.open_dir(to_name)?;
        for name in &path {
            dir = dir.open_dir(name)?;
        }
        dir.set_mode(mode)?;
    }
    Ok(())
}

fn transfer_one(root: &Root, vp: &str, dest: &Dir, op: TransferOp) -> ApiResult<()> {
    let Some((src_dir, name)) = locate(root, vp)? else {
        return Err(ApiError::forbidden("The root folder cannot be moved"));
    };
    src_dir.lstat(&name)?;
    // Compare real locations, so a symlinked destination cannot point back
    // into the source. The source itself is not followed: a symlink is
    // copied or moved as a link.
    let src_real = src_dir.real_path()?.join(&name);
    if dest.real_path()?.starts_with(&src_real) {
        return Err(ApiError::bad_request("A folder cannot be copied or moved into itself"));
    }
    if op == TransferOp::Move && src_dir.stat()?.same_file(&dest.stat()?) {
        return Ok(()); // Moving onto itself: nothing to do.
    }
    if name == crate::trash::DIR && root.is_root(dest) {
        return Err(ApiError::bad_request("That name is reserved for the Trash"));
    }
    match op {
        TransferOp::Copy => copy(&src_dir, &name, dest, &unique_name(dest, &name))?,
        TransferOp::Move => match rename_unique(&src_dir, &name, dest, &name) {
            Ok(_) => {}
            // Different filesystems (e.g. two volumes): copy then delete.
            // `copy` cleans up a partial copy if it fails.
            Err(e) if confined::errno(&e) == Some(Errno::XDEV) => {
                copy(&src_dir, &name, dest, &unique_name(dest, &name))?;
                src_dir.remove(&name)?;
            }
            Err(e) => return Err(e.into()),
        },
    }
    Ok(())
}

pub fn transfer(root: &Root, req: &TransferReq) -> ApiResult<OpReport> {
    let dest = root.open_dir(&components(&req.dest)?).map_err(|e| match e.kind() {
        io::ErrorKind::NotADirectory => ApiError::bad_request("Destination is not a folder"),
        _ => e.into(),
    })?;
    let mut report = OpReport::default();
    for vp in &req.paths {
        match transfer_one(root, vp, &dest, req.op) {
            Ok(()) => report.done += 1,
            Err(e) => report.errors.push(format!("{}: {}", label(vp), e.message)),
        }
    }
    Ok(report)
}

/// Folders still to walk: (parent, name, virtual path). Each is opened when
/// its turn comes, so only the parents of pending folders stay open.
type Todo = Vec<(Rc<Dir>, OsString, String)>;

/// Case-insensitive recursive name search below `virt`. Does not follow
/// symlinked folders (avoids loops).
pub fn search(root: &Root, virt: &str, query: &str, limit: usize) -> ApiResult<Vec<Entry>> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let start = open_dir(root, virt)?;
    let mut next = Some((Rc::new(start), normalize_virtual(virt)?));
    let mut todo: Todo = Vec::new();
    let mut out = Vec::new();
    let mut visited = 0usize;
    loop {
        let (dir, vdir) = match next.take() {
            Some(start) => start,
            None => match todo.pop() {
                Some((parent, name, vpath)) => match parent.open_dir(&name) {
                    Ok(d) => (Rc::new(d), vpath),
                    Err(_) => continue,
                },
                None => break,
            },
        };
        let in_root = root.is_root(&dir);
        let Ok(entries) = dir.entries() else { continue };
        for (name, kind) in entries {
            visited += 1;
            if visited > WALK_LIMIT {
                return Ok(out);
            }
            let shown = name.to_string_lossy().into_owned();
            if (in_root && shown == crate::trash::DIR) || is_upload_temp(&shown) {
                continue;
            }
            let vpath = vjoin(&vdir, &shown);
            if shown.to_lowercase().contains(&needle) {
                if let Some(e) = entry_for(root, &dir, &name, vpath.clone()) {
                    out.push(e);
                    if out.len() >= limit {
                        return Ok(out);
                    }
                }
            }
            if kind == FileType::Directory {
                todo.push((dir.clone(), name, vpath));
            }
        }
    }
    Ok(out)
}

/// Total size of a file or folder tree (symlinks are not followed).
pub fn size_of(root: &Root, virt: &str) -> ApiResult<SizeReport> {
    let mut report = SizeReport::default();
    let start = match locate(root, virt)? {
        None => root.dir().try_clone()?,
        Some((dir, name)) => {
            let meta = dir.lstat(&name)?;
            if !meta.is_dir() {
                report.bytes = meta.len;
                report.files = 1;
                return Ok(report);
            }
            dir.open_dir(&name)?
        }
    };
    let mut next = Some(Rc::new(start));
    let mut todo: Vec<(Rc<Dir>, OsString)> = Vec::new();
    let mut visited = 0usize;
    loop {
        let dir = match next.take() {
            Some(start) => start,
            None => match todo.pop() {
                Some((parent, name)) => match parent.open_dir(&name) {
                    Ok(d) => Rc::new(d),
                    Err(_) => continue,
                },
                None => break,
            },
        };
        let in_root = root.is_root(&dir);
        let Ok(entries) = dir.entries() else { continue };
        for (name, kind) in entries {
            visited += 1;
            if visited > WALK_LIMIT {
                return Ok(report);
            }
            if in_root && name == crate::trash::DIR {
                continue;
            }
            if kind == FileType::Directory {
                report.dirs += 1;
                todo.push((dir.clone(), name));
            } else if let Ok(m) = dir.lstat(&name) {
                report.files += 1;
                report.bytes += m.len;
            }
        }
    }
    Ok(report)
}

pub fn disk_info(root: &Root) -> Option<DiskInfo> {
    let s = rustix::fs::fstatvfs(root.dir()).ok()?;
    Some(DiskInfo { total: s.f_blocks.saturating_mul(s.f_frsize), available: s.f_bavail.saturating_mul(s.f_frsize) })
}

/// Sidebar places: configured ones if any, otherwise the root plus its
/// top-level (non-hidden) folders.
pub fn places(root: &Root, root_name: &str, configured: &[Place]) -> Vec<Place> {
    let mut out = vec![Place { name: root_name.to_string(), path: "/".into() }];
    if !configured.is_empty() {
        out.extend(configured.iter().filter(|p| open_dir(root, &p.path).is_ok()).cloned());
        return out;
    }
    let mut dirs: Vec<String> = root
        .dir()
        .entries()
        .unwrap_or_default()
        .into_iter()
        .filter(|(name, kind)| match kind {
            FileType::Directory => true,
            FileType::Symlink => root.open_dir(&[name]).is_ok(),
            _ => false,
        })
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.') && n != "lost+found")
        .collect();
    dirs.sort_by_key(|d| d.to_lowercase());
    out.extend(dirs.into_iter().take(24).map(|n| Place { path: format!("/{n}"), name: n }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    fn setup() -> (tempfile::TempDir, PathBuf, Root) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(path.join("docs/sub")).unwrap();
        fs::write(path.join("docs/a.txt"), b"hello").unwrap();
        fs::write(path.join("docs/sub/b.txt"), b"world!").unwrap();
        let root = Root::open(&path, crate::trash::DIR).unwrap();
        (tmp, path, root)
    }

    fn os(s: &str) -> &OsStr {
        OsStr::new(s)
    }

    #[test]
    fn identical_files_are_found_by_content() {
        let (_t, path, root) = setup();
        let dir = open_dir(&root, "/docs").unwrap();
        let hash = file_sha256(fs::File::open(path.join("docs/a.txt")).unwrap()).unwrap();
        assert_eq!(hash, "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
        assert_eq!(find_identical(&dir, os("a.txt"), 5, &hash), Some("a.txt".into()));
        // Another file under the name: the copy is found as "a (1).txt".
        fs::write(path.join("docs/a.txt"), b"other").unwrap();
        assert_eq!(find_identical(&dir, os("a.txt"), 5, &hash), None);
        fs::write(path.join("docs/a (1).txt"), b"hello").unwrap();
        assert_eq!(find_identical(&dir, os("a.txt"), 5, &hash), Some("a (1).txt".into()));
        assert_eq!(find_identical(&dir, os("new.txt"), 5, &hash), None);
    }

    #[test]
    fn large_files_hash_like_a_one_shot_digest() {
        use sha2::{Digest, Sha256};
        let t = tempfile::tempdir().unwrap();
        // Several read buffers' worth, and not a whole number of them.
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        fs::write(t.path().join("big"), &data).unwrap();
        let hash = file_sha256(fs::File::open(t.path().join("big")).unwrap()).unwrap();
        assert_eq!(hash, hex(&Sha256::digest(&data)));
    }

    #[test]
    fn upload_temps_are_hidden_and_stale_ones_removed() {
        let (_t, path, root) = setup();
        let (_f, live) = create_temp(&open_dir(&root, "/docs").unwrap()).unwrap();
        let stale = path.join("docs/.narwhal-upload-1-1.part");
        fs::write(&stale, b"x").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400);
        fs::File::options().write(true).open(&stale).unwrap().set_modified(old).unwrap();
        let names: Vec<_> = list_dir(&root, "/docs").unwrap().entries.into_iter().map(|e| e.name).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(path.join("docs").join(live).exists());
        assert!(!stale.exists());
    }

    #[test]
    fn traversal_is_rejected() {
        let (_t, _path, root) = setup();
        assert!(open_file(&root, "/../etc/passwd").is_err());
        assert!(open_file(&root, "docs/../../x").is_err());
        assert!(open_file(&root, "/docs/a.txt").is_ok());
    }

    #[test]
    fn symlink_escape_is_rejected() {
        let (_t, path, root) = setup();
        symlink("/etc", path.join("evil")).unwrap();
        symlink("../../etc/passwd", path.join("docs/passwd")).unwrap();
        assert!(open_file(&root, "/evil/passwd").is_err());
        assert!(open_file(&root, "/docs/passwd").is_err());
        assert!(list_dir(&root, "/evil").is_err());
        assert!(make_dirs(&root, "/evil", &[]).is_err());
    }

    #[test]
    fn listings_do_not_describe_targets_outside_the_root() {
        let (tmp, path, root) = setup();
        let outside = tempfile::tempdir_in(tmp.path().parent().unwrap()).unwrap();
        fs::write(outside.path().join("secret"), b"1234567").unwrap();
        symlink(outside.path().join("secret"), path.join("docs/out")).unwrap();
        symlink("a.txt", path.join("docs/in")).unwrap();
        let l = list_dir(&root, "/docs").unwrap();
        let get = |n: &str| l.entries.iter().find(|e| e.name == n).unwrap().clone();
        let (out, inside) = (get("out"), get("in"));
        assert!(out.symlink && out.kind == EntryKind::Other && out.size == 0 && out.modified.is_none());
        assert!(inside.symlink && inside.kind == EntryKind::File && inside.size == 5);
    }

    #[test]
    fn list_and_sizes() {
        let (_t, _path, root) = setup();
        let l = list_dir(&root, "/docs").unwrap();
        assert_eq!(l.path, "/docs");
        assert_eq!(l.entries.len(), 2);
        let s = size_of(&root, "/docs").unwrap();
        assert_eq!(s.bytes, 11);
        assert_eq!(s.files, 2);
        assert_eq!(s.dirs, 1);
        let s = size_of(&root, "/").unwrap();
        assert_eq!((s.bytes, s.files, s.dirs), (11, 2, 2));
        assert_eq!(size_of(&root, "/docs/a.txt").unwrap().bytes, 5);
    }

    #[test]
    fn copy_move_rename_delete() {
        let (_t, path, root) = setup();
        let r = transfer(
            &root,
            &TransferReq { paths: vec!["/docs/a.txt".into()], dest: "/docs".into(), op: TransferOp::Copy },
        )
        .unwrap();
        assert_eq!(r.done, 1);
        assert!(path.join("docs/a (1).txt").exists());

        let r = transfer(
            &root,
            &TransferReq { paths: vec!["/docs".into()], dest: "/docs/sub".into(), op: TransferOp::Move },
        )
        .unwrap();
        assert_eq!(r.errors.len(), 1, "moving a folder into itself must fail");

        rename(&root, &RenameReq { path: "/docs/sub".into(), new_name: "renamed".into() }).unwrap();
        assert!(path.join("docs/renamed/b.txt").exists());
        assert!(rename(&root, &RenameReq { path: "/docs/renamed".into(), new_name: "a.txt".into() }).is_err());

        let r = delete(&root, &["/docs/renamed".into(), "/".into()]);
        assert_eq!(r.done, 1);
        assert_eq!(r.errors.len(), 1, "root must not be deletable");
        assert!(!path.join("docs/renamed").exists());
    }

    #[test]
    fn search_finds_nested() {
        let (_t, _path, root) = setup();
        let hits = search(&root, "/", "B.TXT", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "/docs/sub/b.txt");
    }

    #[test]
    fn case_only_rename_does_not_clobber_another_file() {
        let (_t, path, root) = setup();
        fs::write(path.join("Foo"), b"one").unwrap();
        fs::write(path.join("foo"), b"two").unwrap();
        let r = rename(&root, &RenameReq { path: "/Foo".into(), new_name: "foo".into() });
        assert_eq!(r.unwrap_err().status, axum::http::StatusCode::CONFLICT);
        assert_eq!(fs::read(path.join("Foo")).unwrap(), b"one");
        assert_eq!(fs::read(path.join("foo")).unwrap(), b"two");
    }

    #[test]
    fn case_only_rename_works_when_free() {
        let (_t, path, root) = setup();
        fs::write(path.join("Foo"), b"one").unwrap();
        let p = rename(&root, &RenameReq { path: "/Foo".into(), new_name: "foo".into() }).unwrap();
        assert_eq!(p, "/foo");
        assert_eq!(fs::read(path.join("foo")).unwrap(), b"one");
    }

    #[test]
    fn rename_noreplace_refuses_existing_target() {
        let (_t, path, root) = setup();
        let (docs, sub) = (open_dir(&root, "/docs").unwrap(), open_dir(&root, "/docs/sub").unwrap());
        let err = docs.rename_noreplace(os("a.txt"), &sub, os("b.txt")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(path.join("docs/sub/b.txt")).unwrap(), b"world!");
        assert!(path.join("docs/a.txt").exists());
    }

    #[test]
    fn rename_unique_picks_a_free_name() {
        let (_t, path, root) = setup();
        fs::write(path.join("up.part"), b"new").unwrap();
        let docs = open_dir(&root, "/docs").unwrap();
        let t = rename_unique(root.dir(), os("up.part"), &docs, os("a.txt")).unwrap();
        assert_eq!(t, "a (1).txt");
        assert_eq!(fs::read(path.join("docs/a.txt")).unwrap(), b"hello");
        assert_eq!(fs::read(path.join("docs/a (1).txt")).unwrap(), b"new");
    }

    #[test]
    fn copy_into_itself_through_symlink_is_rejected() {
        let (_t, path, root) = setup();
        symlink("docs", path.join("link")).unwrap();
        for op in [TransferOp::Copy, TransferOp::Move] {
            let r = transfer(&root, &TransferReq { paths: vec!["/docs".into()], dest: "/link".into(), op }).unwrap();
            assert_eq!((r.done, r.errors.len()), (0, 1), "{op:?} into itself via a link must fail");
        }
        let r = transfer(
            &root,
            &TransferReq { paths: vec!["/docs".into()], dest: "/link/sub".into(), op: TransferOp::Copy },
        )
        .unwrap();
        assert_eq!(r.errors.len(), 1);
        assert!(!path.join("docs/docs").exists());
        assert!(!path.join("docs/sub/docs").exists());
    }

    #[test]
    fn moving_a_symlink_moves_the_link_itself() {
        let (_t, path, root) = setup();
        symlink("docs", path.join("docs/sub/up")).unwrap();
        // The link points at an ancestor, but moving the link (not its target) is fine.
        let r = transfer(
            &root,
            &TransferReq { paths: vec!["/docs/sub/up".into()], dest: "/".into(), op: TransferOp::Move },
        )
        .unwrap();
        assert_eq!(r.done, 1, "{:?}", r.errors);
        assert!(fs::symlink_metadata(path.join("up")).unwrap().file_type().is_symlink());
    }

    #[test]
    fn the_trash_folder_name_cannot_be_moved_into_the_root() {
        let (_t, path, root) = setup();
        fs::create_dir(path.join("docs").join(crate::trash::DIR)).unwrap();
        for op in [TransferOp::Copy, TransferOp::Move] {
            let req = TransferReq { paths: vec![format!("/docs/{}", crate::trash::DIR)], dest: "/".into(), op };
            assert_eq!(transfer(&root, &req).unwrap().errors.len(), 1, "{op:?}");
        }
        assert!(!path.join(crate::trash::DIR).exists());
    }

    #[test]
    fn copy_preserves_tree_links_and_permissions() {
        let (_t, path, root) = setup();
        symlink("a.txt", path.join("docs/ln")).unwrap();
        fs::set_permissions(path.join("docs/a.txt"), fs::Permissions::from_mode(0o640)).unwrap();
        fs::set_permissions(path.join("docs/sub"), fs::Permissions::from_mode(0o555)).unwrap();
        copy(root.dir(), os("docs"), root.dir(), os("copy")).unwrap();
        assert_eq!(fs::read(path.join("copy/sub/b.txt")).unwrap(), b"world!");
        assert_eq!(fs::read_link(path.join("copy/ln")).unwrap(), Path::new("a.txt"));
        let mode = |p: &str| fs::metadata(path.join(p)).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode("copy/a.txt"), 0o640);
        assert_eq!(mode("copy/sub"), 0o555);
        assert_eq!(mode("copy"), mode("docs"));
        fs::set_permissions(path.join("docs/sub"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(path.join("copy/sub"), fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn copies_are_never_setuid() {
        let (_t, path, root) = setup();
        fs::set_permissions(path.join("docs/a.txt"), fs::Permissions::from_mode(0o4755)).unwrap();
        let docs = open_dir(&root, "/docs").unwrap();
        copy(&docs, os("a.txt"), &docs, os("c.txt")).unwrap();
        assert_eq!(fs::metadata(path.join("docs/c.txt")).unwrap().permissions().mode() & 0o7777, 0o755);
    }

    #[test]
    fn copy_handles_deep_trees_without_stack_overflow() {
        let (_t, path, root) = setup();
        let mut deep = path.join("deep");
        for _ in 0..1500 {
            deep.push("d");
        }
        fs::create_dir_all(&deep).unwrap();
        copy(root.dir(), os("deep"), root.dir(), os("deep2")).unwrap();
        assert!(path.join("deep2").join(deep.strip_prefix(path.join("deep")).unwrap()).is_dir());
        root.dir().remove(os("deep2")).unwrap();
        assert!(!path.join("deep2").exists());
    }

    #[test]
    fn failed_copy_cleans_up_and_never_overwrites() {
        let (_t, path, root) = setup();
        rustix::fs::mknodat(
            open_dir(&root, "/docs/sub").unwrap(),
            "pipe",
            FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o644),
            0,
        )
        .unwrap();
        assert!(copy(root.dir(), os("docs"), root.dir(), os("copy")).is_err());
        assert!(!path.join("copy").exists(), "partial copy must be removed");
        // An existing destination is refused and left alone.
        let (docs, sub) = (open_dir(&root, "/docs").unwrap(), open_dir(&root, "/docs/sub").unwrap());
        assert!(copy(&docs, os("a.txt"), &sub, os("b.txt")).is_err());
        assert_eq!(fs::read(path.join("docs/sub/b.txt")).unwrap(), b"world!");
    }

    #[test]
    fn upload_temp_never_follows_a_planted_symlink() {
        let (_t, path, root) = setup();
        let outside = tempfile::tempdir().unwrap();
        // Plant dangling links outside the root under the next temp names.
        let next = TEMP_SEQ.load(Ordering::Relaxed);
        for seq in next..next + 50 {
            let name = format!(".narwhal-upload-{}-{seq}.part", std::process::id());
            symlink(outside.path().join(&name), path.join("docs").join(&name)).unwrap();
        }
        let (_f, tmp) = create_temp(&open_dir(&root, "/docs").unwrap()).unwrap();
        assert!(fs::symlink_metadata(path.join("docs").join(tmp)).unwrap().file_type().is_file());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0, "nothing may be written outside the root");
    }

    #[test]
    fn unique_names() {
        let (_t, _path, root) = setup();
        let docs = open_dir(&root, "/docs").unwrap();
        assert_eq!(unique_name(&docs, os("a.txt")), "a (1).txt");
        assert_eq!(unique_name(&docs, os("new.txt")), "new.txt");
        assert_eq!(name_candidates(os(".bashrc")).nth(1).unwrap(), ".bashrc (1)");
        assert_eq!(name_candidates(os("a.tar.gz")).nth(2).unwrap(), "a.tar (2).gz");
        assert!(sanitize_name("..").is_err());
        assert_eq!(sanitize_name("C:\\x\\y.png").unwrap(), "y.png");
    }

    #[test]
    fn upload_paths() {
        let (dirs, name) = split_upload_path("photos//2024/a.jpg").unwrap();
        assert_eq!(dirs, ["photos", "2024"]);
        assert_eq!(name, "a.jpg");
        assert_eq!(split_upload_path("a.jpg").unwrap(), (vec![], "a.jpg".to_string()));
        assert!(split_upload_path("photos/../../etc/passwd").is_err());
        assert!(split_upload_path("a/./b").is_err());
        assert!(split_upload_path("").is_err());
    }

    #[test]
    fn upload_dirs_stay_inside_root() {
        let (_t, path, root) = setup();
        let same =
            |d: &Dir, p: &str| d.stat().unwrap().same_file(&Dir::open_path(&path.join(p)).unwrap().stat().unwrap());
        let d = make_dirs(&root, "/docs", &["new".into(), "deep".into()]).unwrap();
        assert!(same(&d, "docs/new/deep"));
        // Existing folders are reused, also through a link inside the root.
        assert!(same(&make_dirs(&root, "/docs", &["sub".into()]).unwrap(), "docs/sub"));
        symlink("sub", path.join("docs/alias")).unwrap();
        assert!(same(&make_dirs(&root, "/docs", &["alias".into()]).unwrap(), "docs/sub"));
        // A file in the way is refused.
        let err = make_dirs(&root, "/docs", &["a.txt".into()]).err().unwrap();
        assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
        // A symlink leading out of the root is refused.
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), path.join("docs/out")).unwrap();
        assert!(make_dirs(&root, "/docs", &["out".into(), "x".into()]).is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
