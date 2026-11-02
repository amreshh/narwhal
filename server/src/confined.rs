//! Filesystem access confined to the storage root.
//!
//! A path from a client is never handed to the kernel as a whole: a folder
//! on the way could be swapped for a symlink between checking the path and
//! using it, and the second lookup would then lead out of the root.
//! Instead, folders are opened one name at a time without following
//! symlinks, and everything else works relative to an open folder (the
//! `*at` system calls), so it acts on exactly the folder that was checked.
//! Symlinks are followed here, by hand, and only while they stay inside the
//! root.

use rustix::fs::{self as sys, AtFlags, FileType, Mode, OFlags, RenameFlags};
use rustix::io::Errno;
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

/// Opening a folder only to look things up in it (or a file only to name
/// it) needs no read permission.
#[cfg(any(target_os = "linux", target_os = "android"))]
const LOOKUP: OFlags = OFlags::PATH;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
const LOOKUP: OFlags = OFlags::RDONLY;

/// Most symlinks followed while resolving one path, as in the kernel.
const MAX_LINKS: u32 = 40;

/// A path that leads outside the root.
#[derive(Debug)]
pub struct OutsideRoot;

impl std::fmt::Display for OutsideRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Path is outside the storage root")
    }
}

impl std::error::Error for OutsideRoot {}

fn outside() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, OutsideRoot)
}

pub fn errno(e: &io::Error) -> Option<Errno> {
    Errno::from_io_error(e)
}

/// The parts of `stat` the app uses.
#[derive(Clone, Copy, Debug)]
pub struct Meta {
    pub kind: FileType,
    pub len: u64,
    /// Permission bits (`0o7777`).
    pub mode: u32,
    /// Last modification, seconds since the Unix epoch.
    pub modified: i64,
    dev: u64,
    ino: u64,
}

impl Meta {
    pub fn is_dir(&self) -> bool {
        self.kind == FileType::Directory
    }

    pub fn is_file(&self) -> bool {
        self.kind == FileType::RegularFile
    }

    pub fn is_symlink(&self) -> bool {
        self.kind == FileType::Symlink
    }

    /// The same file or folder, possibly under another name.
    pub fn same_file(&self, other: &Meta) -> bool {
        (self.dev, self.ino) == (other.dev, other.ino)
    }
}

// The field types differ between architectures.
#[allow(clippy::unnecessary_cast)]
impl From<sys::Stat> for Meta {
    fn from(s: sys::Stat) -> Self {
        Meta {
            kind: FileType::from_raw_mode(s.st_mode as _),
            len: s.st_size as u64,
            mode: s.st_mode as u32 & 0o7777,
            modified: s.st_mtime as i64,
            dev: s.st_dev as u64,
            ino: s.st_ino as u64,
        }
    }
}

/// Metadata of an open file or folder.
pub fn fstat(fd: impl AsFd) -> io::Result<Meta> {
    Ok(sys::fstat(fd)?.into())
}

/// A path that opens the very file `fd` refers to again, for APIs that only
/// take paths. Only valid while `fd` stays open.
pub fn fd_path(fd: impl AsFd) -> PathBuf {
    let n = fd.as_fd().as_raw_fd();
    if cfg!(any(target_os = "linux", target_os = "android")) {
        PathBuf::from(format!("/proc/self/fd/{n}"))
    } else {
        PathBuf::from(format!("/dev/fd/{n}"))
    }
}

/// An open folder. Names given to its methods are single path components
/// (no `/`), looked up in this folder wherever it is now. None of them
/// follow a symlink at that name.
#[derive(Debug)]
pub struct Dir(OwnedFd);

impl AsFd for Dir {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl Dir {
    /// Open a folder by its full path (the root, at startup).
    pub fn open_path(path: &Path) -> io::Result<Dir> {
        Ok(Dir(sys::open(path, LOOKUP | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty())?))
    }

    pub fn try_clone(&self) -> io::Result<Dir> {
        Ok(Dir(self.0.try_clone()?))
    }

    /// The sub-folder `name`. A symlink is refused, not followed.
    pub fn open_dir(&self, name: &OsStr) -> io::Result<Dir> {
        let flags = LOOKUP | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        Ok(Dir(sys::openat(&self.0, name, flags, Mode::empty())?))
    }

    /// `name`, which is not a folder, opened only to name it.
    fn open_other(&self, name: &OsStr) -> io::Result<OwnedFd> {
        let flags = LOOKUP | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
        Ok(sys::openat(&self.0, name, flags, Mode::empty())?)
    }

    /// The regular file `name`, opened for reading. Anything else is
    /// refused: a symlink, a device, or a FIFO (which would block).
    pub fn open_file(&self, name: &OsStr) -> io::Result<File> {
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
        let fd = sys::openat(&self.0, name, flags, Mode::empty())?;
        if !fstat(&fd)?.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Not a regular file"));
        }
        Ok(File::from(fd))
    }

    /// Create the file `name` for writing. Fails if anything exists at that
    /// name, even a dangling symlink, so it can't write anywhere else.
    pub fn create_file(&self, name: &OsStr, mode: u32) -> io::Result<File> {
        let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        Ok(File::from(sys::openat(&self.0, name, flags, Mode::from_raw_mode(mode))?))
    }

    pub fn mkdir(&self, name: &OsStr, mode: u32) -> io::Result<()> {
        Ok(sys::mkdirat(&self.0, name, Mode::from_raw_mode(mode))?)
    }

    pub fn symlink(&self, target: &OsStr, name: &OsStr) -> io::Result<()> {
        Ok(sys::symlinkat(target, &self.0, name)?)
    }

    pub fn read_link(&self, name: &OsStr) -> io::Result<OsString> {
        Ok(OsString::from_vec(sys::readlinkat(&self.0, name, Vec::new())?.into_bytes()))
    }

    /// Metadata of `name` itself (a symlink is not followed).
    pub fn lstat(&self, name: &OsStr) -> io::Result<Meta> {
        Ok(sys::statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW)?.into())
    }

    /// Metadata of this folder.
    pub fn stat(&self) -> io::Result<Meta> {
        fstat(&self.0)
    }

    /// This folder, opened for reading its entries or changing it.
    fn open_self(&self) -> io::Result<OwnedFd> {
        Ok(sys::openat(&self.0, ".", OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty())?)
    }

    /// Names and types of the entries (without `.` and `..`).
    pub fn entries(&self) -> io::Result<Vec<(OsString, FileType)>> {
        let mut out = Vec::new();
        for entry in sys::Dir::new(self.open_self()?)? {
            let entry = entry?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            let name = OsStr::from_bytes(name).to_owned();
            let kind = match entry.file_type() {
                // Some filesystems don't say; ask. Gone meanwhile: skip it.
                FileType::Unknown => match self.lstat(&name) {
                    Ok(m) => m.kind,
                    Err(_) => continue,
                },
                kind => kind,
            };
            out.push((name, kind));
        }
        Ok(out)
    }

    /// Number of entries, counting up to `limit`.
    pub fn count_entries(&self, limit: usize) -> io::Result<usize> {
        let mut n = 0;
        for entry in sys::Dir::new(self.open_self()?)? {
            let entry = entry?;
            let name = entry.file_name().to_bytes();
            if name != b"." && name != b".." {
                n += 1;
                if n >= limit {
                    break;
                }
            }
        }
        Ok(n)
    }

    pub fn unlink(&self, name: &OsStr) -> io::Result<()> {
        Ok(sys::unlinkat(&self.0, name, AtFlags::empty())?)
    }

    pub fn rmdir(&self, name: &OsStr) -> io::Result<()> {
        Ok(sys::unlinkat(&self.0, name, AtFlags::REMOVEDIR)?)
    }

    /// Remove `name`: a file, a symlink (not what it points to), or a folder
    /// with everything in it. Iterative, so deep trees cannot overflow the
    /// stack; it keeps one folder per level open.
    pub fn remove(&self, name: &OsStr) -> io::Result<()> {
        if !self.lstat(name)?.is_dir() {
            return self.unlink(name);
        }
        struct Level {
            dir: Dir,
            name: OsString,
            subdirs: Vec<OsString>,
        }
        // Remove the files in a folder; return it with its sub-folders.
        fn enter(dir: Dir, name: OsString) -> io::Result<Level> {
            let mut subdirs = Vec::new();
            for (child, kind) in dir.entries()? {
                if kind == FileType::Directory {
                    subdirs.push(child);
                } else {
                    ignore_missing(dir.unlink(&child))?;
                }
            }
            Ok(Level { dir, name, subdirs })
        }
        let mut levels = vec![enter(self.open_dir(name)?, name.to_owned())?];
        while let Some(top) = levels.last_mut() {
            if let Some(sub) = top.subdirs.pop() {
                match top.dir.open_dir(&sub) {
                    Ok(dir) => {
                        let level = enter(dir, sub)?;
                        levels.push(level);
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            } else {
                let done = levels.pop().expect("a level is open");
                let parent = levels.last().map_or(self, |l| &l.dir);
                ignore_missing(parent.rmdir(&done.name))?;
            }
        }
        Ok(())
    }

    /// Rename `name` to `to_name` in `to`, failing with `AlreadyExists`
    /// instead of replacing an existing entry. Atomic where the filesystem
    /// supports `RENAME_NOREPLACE`.
    pub fn rename_noreplace(&self, name: &OsStr, to: &Dir, to_name: &OsStr) -> io::Result<()> {
        match sys::renameat_with(&self.0, name, &to.0, to_name, RenameFlags::NOREPLACE) {
            Ok(()) => Ok(()),
            // Filesystems without RENAME_NOREPLACE: check, then rename.
            Err(Errno::INVAL | Errno::NOSYS | Errno::OPNOTSUPP) => {
                if to.lstat(to_name).is_ok() {
                    return Err(io::ErrorKind::AlreadyExists.into());
                }
                self.rename(name, to, to_name)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Rename `name` to `to_name` in `to`, replacing what is there.
    pub fn rename(&self, name: &OsStr, to: &Dir, to_name: &OsStr) -> io::Result<()> {
        Ok(sys::renameat(&self.0, name, &to.0, to_name)?)
    }

    /// Set this folder's permission bits.
    pub fn set_mode(&self, mode: u32) -> io::Result<()> {
        Ok(sys::fchmod(self.open_self()?, Mode::from_raw_mode(mode))?)
    }

    /// Where this folder is now, as a full path.
    pub fn real_path(&self) -> io::Result<PathBuf> {
        std::fs::read_link(fd_path(self))
    }
}

/// `Ok` if the entry was already gone.
fn ignore_missing(res: io::Result<()>) -> io::Result<()> {
    match res {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        res => res,
    }
}

/// What a path leads to.
pub enum Found {
    Dir(Dir),
    /// Not a folder (a file, or a special file), opened only to name it.
    Other(OwnedFd),
}

/// The storage root, held open: every path is resolved from here.
pub struct Root {
    /// Canonical; absolute symlinks into the root are recognised by it.
    path: PathBuf,
    dir: Dir,
    meta: Meta,
    /// A name in the root that paths may not lead to.
    hidden: &'static str,
}

impl Root {
    /// Open `path` as the root. `hidden` names an entry of the root that no
    /// path may reach (the Trash, which has its own API).
    pub fn open(path: &Path, hidden: &'static str) -> io::Result<Root> {
        let path = path.canonicalize()?;
        let dir = Dir::open_path(&path)?;
        let meta = dir.stat()?;
        Ok(Root { path, dir, meta, hidden })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn dir(&self) -> &Dir {
        &self.dir
    }

    /// Whether `dir` is the root folder itself.
    pub fn is_root(&self, dir: &Dir) -> bool {
        dir.stat().is_ok_and(|m| m.same_file(&self.meta))
    }

    /// The folder `names` (plain names, no `..`) leads to.
    pub fn open_dir<S: AsRef<OsStr>>(&self, names: &[S]) -> io::Result<Dir> {
        match self.find(names)? {
            Found::Dir(dir) => Ok(dir),
            Found::Other(_) => Err(io::ErrorKind::NotADirectory.into()),
        }
    }

    /// What `names` (plain names, no `..`) leads to. Symlinks are followed
    /// as long as they stay inside the root; one that leads out is refused.
    pub fn find<S: AsRef<OsStr>>(&self, names: &[S]) -> io::Result<Found> {
        let mut queue: VecDeque<OsString> = names.iter().map(|n| n.as_ref().to_owned()).collect();
        // The folders entered below the root; the last one is where we are.
        let mut stack: Vec<Dir> = Vec::new();
        let mut links = 0;
        while let Some(name) = queue.pop_front() {
            if name.is_empty() || name == "." {
                continue;
            }
            if name == ".." {
                // (Only from link targets.) Back to the folder we came from.
                stack.pop().ok_or_else(outside)?;
                continue;
            }
            if stack.is_empty() && name == self.hidden {
                return Err(io::ErrorKind::NotFound.into());
            }
            let here = stack.last().unwrap_or(&self.dir);
            let err = match here.open_dir(&name) {
                Ok(dir) => {
                    stack.push(dir);
                    continue;
                }
                Err(e) => e,
            };
            // Not a folder: a symlink, or a file.
            if !matches!(errno(&err), Some(Errno::NOTDIR | Errno::LOOP)) {
                return Err(err);
            }
            let target = match here.read_link(&name) {
                Ok(target) => PathBuf::from(target),
                // A file: fine at the end of the path.
                Err(_) if queue.is_empty() => return Ok(Found::Other(here.open_other(&name)?)),
                Err(_) => return Err(err),
            };
            links += 1;
            if links > MAX_LINKS {
                return Err(Errno::LOOP.into());
            }
            let target = if target.is_absolute() {
                // Absolute links are fine if they point into the root.
                let inside = target.strip_prefix(&self.path).map_err(|_| outside())?.to_path_buf();
                stack.clear();
                inside
            } else {
                target
            };
            for c in target.components().rev() {
                match c {
                    Component::Normal(s) => queue.push_front(s.to_owned()),
                    Component::ParentDir => queue.push_front("..".into()),
                    Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
                }
            }
        }
        Ok(Found::Dir(match stack.pop() {
            Some(dir) => dir,
            None => self.dir.try_clone()?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn setup() -> (tempfile::TempDir, PathBuf, Root) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("root");
        fs::create_dir_all(path.join("a/b")).unwrap();
        fs::write(path.join("a/b/f.txt"), b"inside").unwrap();
        fs::write(tmp.path().join("secret.txt"), b"outside").unwrap();
        let root = Root::open(&path, ".hidden").unwrap();
        (tmp, path, root)
    }

    fn read(root: &Root, names: &[&str]) -> io::Result<String> {
        match root.find(names)? {
            Found::Other(fd) => fs::read_to_string(fd_path(&fd)),
            Found::Dir(_) => Err(io::ErrorKind::IsADirectory.into()),
        }
    }

    #[test]
    fn links_inside_the_root_are_followed() {
        let (_t, path, root) = setup();
        symlink("a/b", path.join("rel")).unwrap();
        symlink(path.join("a"), path.join("abs")).unwrap();
        symlink("../b/f.txt", path.join("a/b/up")).unwrap();
        assert_eq!(read(&root, &["rel", "f.txt"]).unwrap(), "inside");
        assert_eq!(read(&root, &["abs", "b", "f.txt"]).unwrap(), "inside");
        assert_eq!(read(&root, &["a", "b", "up"]).unwrap(), "inside");
        assert!(root.is_root(&root.open_dir::<&str>(&[]).unwrap()));
    }

    #[test]
    fn links_out_of_the_root_are_refused() {
        let (tmp, path, root) = setup();
        symlink("../secret.txt", path.join("rel")).unwrap();
        symlink(tmp.path().join("secret.txt"), path.join("abs")).unwrap();
        symlink("../..", path.join("a/b/top")).unwrap();
        for names in [&["rel"][..], &["abs"], &["a", "b", "top", "..", "secret.txt"]] {
            let err = read(&root, names).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{names:?}");
        }
        // Loops end.
        symlink("loop", path.join("loop")).unwrap();
        assert!(root.find(&["loop"]).is_err());
    }

    #[test]
    fn the_hidden_entry_is_unreachable_even_through_links() {
        let (_t, path, root) = setup();
        fs::create_dir(path.join(".hidden")).unwrap();
        fs::create_dir(path.join("a/.hidden")).unwrap();
        symlink(".hidden", path.join("h")).unwrap();
        symlink(".", path.join("self")).unwrap();
        assert!(root.find(&[".hidden"]).is_err());
        assert!(root.find(&["h"]).is_err());
        assert!(root.find(&["self", ".hidden"]).is_err());
        assert!(root.find(&["a", ".hidden"]).is_ok(), "only the root's own entry is special");
    }

    /// The race behind the fix: a folder that was checked is swapped for a
    /// symlink out of the root before it is used. An open `Dir` keeps
    /// pointing at the folder it opened.
    #[test]
    fn a_folder_swapped_for_a_link_after_opening_is_not_followed() {
        let (tmp, path, root) = setup();
        let b = root.open_dir(&["a", "b"]).unwrap();
        fs::rename(path.join("a/b"), path.join("a/old")).unwrap();
        symlink(tmp.path(), path.join("a/b")).unwrap();
        // Still the original folder...
        let mut text = String::new();
        io::Read::read_to_string(&mut b.open_file(OsStr::new("f.txt")).unwrap(), &mut text).unwrap();
        assert_eq!(text, "inside");
        assert!(b.open_file(OsStr::new("secret.txt")).is_err());
        // ...and a fresh lookup refuses the link.
        assert_eq!(root.find(&["a", "b", "secret.txt"]).err().unwrap().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn remove_deletes_trees_but_not_link_targets() {
        let (_t, path, root) = setup();
        symlink(path.join("a/b"), path.join("a/link")).unwrap();
        fs::create_dir_all(path.join("t/x/y")).unwrap();
        fs::write(path.join("t/x/y/z"), b"z").unwrap();
        symlink(path.join("a"), path.join("t/x/l")).unwrap();
        root.dir().remove(OsStr::new("t")).unwrap();
        assert!(!path.join("t").exists());
        root.open_dir(&["a"]).unwrap().remove(OsStr::new("link")).unwrap();
        assert!(path.join("a/b/f.txt").exists());
    }
}
