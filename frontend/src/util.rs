use narwhal_shared::{Entry, EntryKind};
use std::cmp::Ordering;
use wasm_bindgen::JsValue;

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", UNITS[i])
    } else {
        format!("{v:.0} {}", UNITS[i])
    }
}

pub fn format_items(n: u32) -> String {
    if n == 1 {
        "1 item".into()
    } else {
        format!("{n} items")
    }
}

pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "Today at 14:03", "Yesterday at 09:12", or "2024-05-17 18:44".
pub fn format_date(secs: i64) -> String {
    let d = js_sys::Date::new(&JsValue::from_f64(secs as f64 * 1000.0));
    let now = js_sys::Date::new_0();
    let yesterday = js_sys::Date::new(&JsValue::from_f64(now.get_time() - 86_400_000.0));
    let same_day = |a: &js_sys::Date, b: &js_sys::Date| {
        a.get_full_year() == b.get_full_year() && a.get_month() == b.get_month() && a.get_date() == b.get_date()
    };
    let hm = format!("{:02}:{:02}", d.get_hours(), d.get_minutes());
    if same_day(&d, &now) {
        format!("Today at {hm}")
    } else if same_day(&d, &yesterday) {
        format!("Yesterday at {hm}")
    } else {
        format!("{}-{:02}-{:02} {hm}", d.get_full_year(), d.get_month() + 1, d.get_date())
    }
}

pub fn mode_string(mode: u32, dir: bool) -> String {
    let mut s = String::with_capacity(10);
    s.push(if dir { 'd' } else { '-' });
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    format!("{s} ({:o})", mode & 0o7777)
}

/// Lower-cased extension without the dot ("" if none). Dotfiles have none.
pub fn ext(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => name[i + 1..].to_lowercase(),
        _ => String::new(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cat {
    Folder,
    Image,
    Video,
    Audio,
    Archive,
    Code,
    Text,
    Pdf,
    Document,
    Spreadsheet,
    Presentation,
    Font,
    DiskImage,
    Other,
}

pub fn category(e: &Entry) -> Cat {
    if e.kind == EntryKind::Dir {
        return Cat::Folder;
    }
    match ext(&e.name).as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "ico" | "tif" | "tiff" | "avif"
        | "heic" | "psd" | "xcf" | "raw" | "cr2" | "nef" => Cat::Image,
        "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "wmv" | "flv" | "mpg" | "mpeg" => Cat::Video,
        "mp3" | "flac" | "ogg" | "opus" | "wav" | "m4a" | "aac" | "wma" | "aiff" => Cat::Audio,
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "deb" | "rpm" | "jar"
        | "apk" => Cat::Archive,
        "rs" | "js" | "ts" | "tsx" | "jsx" | "py" | "go" | "c" | "h" | "cpp" | "hpp" | "java" | "kt"
        | "rb" | "php" | "sh" | "bash" | "zsh" | "html" | "css" | "scss" | "json" | "yaml" | "yml"
        | "toml" | "xml" | "sql" | "lua" | "swift" | "cs" | "vue" | "svelte" | "dockerfile" | "tf"
        | "nix" => Cat::Code,
        "txt" | "md" | "log" | "csv" | "tsv" | "ini" | "conf" | "cfg" | "env" | "rst" | "org" => Cat::Text,
        "pdf" => Cat::Pdf,
        "doc" | "docx" | "odt" | "rtf" | "pages" | "epub" => Cat::Document,
        "xls" | "xlsx" | "ods" | "numbers" => Cat::Spreadsheet,
        "ppt" | "pptx" | "odp" | "key" => Cat::Presentation,
        "ttf" | "otf" | "woff" | "woff2" => Cat::Font,
        "iso" | "img" | "qcow2" | "vmdk" | "vdi" | "dmg" => Cat::DiskImage,
        _ => Cat::Other,
    }
}

pub fn cat_color(c: Cat) -> &'static str {
    match c {
        Cat::Folder => "#3a8fd9",
        Cat::Image => "#d94f86",
        Cat::Video => "#8a5bd6",
        Cat::Audio => "#e8891c",
        Cat::Archive => "#9c7347",
        Cat::Code => "#2f8fcf",
        Cat::Text => "#7d8a96",
        Cat::Pdf => "#d63c34",
        Cat::Document => "#2b6fd6",
        Cat::Spreadsheet => "#22a05a",
        Cat::Presentation => "#e0612a",
        Cat::Font => "#4a5866",
        Cat::DiskImage => "#5c6b7a",
        Cat::Other => "#8994a0",
    }
}

pub fn type_label(e: &Entry) -> String {
    if e.kind == EntryKind::Other {
        return if e.symlink { "Broken link".into() } else { "Special file".into() };
    }
    let x = ext(&e.name).to_uppercase();
    let base = match category(e) {
        Cat::Folder => return if e.symlink { "Folder (link)".into() } else { "Folder".into() },
        Cat::Image => format!("{x} image"),
        Cat::Video => format!("{x} video"),
        Cat::Audio => format!("{x} audio"),
        Cat::Archive => format!("{x} archive"),
        Cat::Code => format!("{x} source"),
        Cat::Text if x == "MD" => "Markdown document".into(),
        Cat::Text => format!("{x} text file"),
        Cat::Pdf => "PDF document".into(),
        Cat::Document => format!("{x} document"),
        Cat::Spreadsheet => format!("{x} spreadsheet"),
        Cat::Presentation => format!("{x} presentation"),
        Cat::Font => format!("{x} font"),
        Cat::DiskImage => format!("{x} disk image"),
        Cat::Other if x.is_empty() => "File".into(),
        Cat::Other => format!("{x} file"),
    };
    if e.symlink {
        format!("{base} (link)")
    } else {
        base
    }
}

/// Images the browser can render directly, small enough to use as thumbnails.
pub fn is_thumbnailable(e: &Entry) -> bool {
    category(e) == Cat::Image
        && matches!(ext(&e.name).as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "ico" | "avif")
        && e.size <= 25 * 1024 * 1024
}

/// Size column text: item count for folders, bytes for files.
pub fn size_text(e: &Entry) -> String {
    match e.kind {
        EntryKind::Dir => e.children.map(format_items).unwrap_or_default(),
        EntryKind::File => format_size(e.size),
        EntryKind::Other => String::new(),
    }
}

/// Case-insensitive natural ordering: "file2" < "file10".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let a = a.to_lowercase();
    let b = b.to_lowercase();
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = ai.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(c) = bi.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    bi.next();
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

pub fn file_list(fl: &web_sys::FileList) -> Vec<web_sys::File> {
    (0..fl.length()).filter_map(|i| fl.get(i)).collect()
}

/// Files from a file input, each with its path relative to the upload
/// folder: just the name, or `folder/sub/name` when a folder was picked.
pub fn upload_items(fl: &web_sys::FileList) -> Vec<(String, web_sys::File)> {
    file_list(fl)
        .into_iter()
        .map(|f| {
            let rel = js_sys::Reflect::get(&f, &"webkitRelativePath".into())
                .ok()
                .and_then(|v| v.as_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| f.name());
            (rel, f)
        })
        .collect()
}

/// Entries of a drop, which may include folders. Must be called while the
/// drop event is being handled: the browser clears the items afterwards.
pub fn dropped_entries(dt: &web_sys::DataTransfer) -> Vec<web_sys::FileSystemEntry> {
    let items = dt.items();
    (0..items.length())
        .filter_map(|i| items.get(i))
        .filter(|it| it.kind() == "file")
        .filter_map(|it| it.webkit_get_as_entry().ok().flatten())
        .collect()
}

/// Walk dropped entries, descending into folders, and return every file
/// with its relative path. Empty folders are skipped.
pub async fn read_entries(
    entries: Vec<web_sys::FileSystemEntry>,
) -> Result<Vec<(String, web_sys::File)>, String> {
    use js_sys::{Array, Promise};
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{FileSystemDirectoryEntry, FileSystemEntry, FileSystemFileEntry};

    let fail = |e: wasm_bindgen::JsValue| {
        let msg = js_sys::Reflect::get(&e, &"message".into()).ok().and_then(|m| m.as_string());
        format!("Could not read the dropped items: {}", msg.unwrap_or_else(|| format!("{e:?}")))
    };
    let mut out = Vec::new();
    let mut stack: Vec<(String, FileSystemEntry)> = entries.into_iter().map(|e| (e.name(), e)).collect();
    while let Some((path, entry)) = stack.pop() {
        if entry.is_directory() {
            let reader = entry.unchecked_ref::<FileSystemDirectoryEntry>().create_reader();
            // readEntries returns results in batches; an empty one means done.
            loop {
                let batch = JsFuture::from(Promise::new(&mut |resolve, reject| {
                    if let Err(e) = reader.read_entries_with_callback_and_callback(&resolve, &reject) {
                        let _ = reject.call1(&wasm_bindgen::JsValue::NULL, &e);
                    }
                }))
                .await
                .map_err(fail)?
                .unchecked_into::<Array>();
                if batch.length() == 0 {
                    break;
                }
                for e in batch.iter() {
                    let e: FileSystemEntry = e.unchecked_into();
                    stack.push((format!("{path}/{}", e.name()), e));
                }
            }
        } else if entry.is_file() {
            let file_entry = entry.unchecked_ref::<FileSystemFileEntry>();
            let file = JsFuture::from(Promise::new(&mut |resolve, reject| {
                file_entry.file_with_callback_and_callback(&resolve, &reject)
            }))
            .await
            .map_err(fail)?;
            out.push((path, file.unchecked_into()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// UTF-16 length of the name without its extension (for selecting it in an input).
pub fn stem_utf16_len(name: &str) -> u32 {
    let stem = match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    };
    stem.encode_utf16().count() as u32
}

/// Every folder from the root down to `path`: "/a/b" -> ["/", "/a", "/a/b"].
pub fn ancestors(path: &str) -> Vec<String> {
    let mut out = vec!["/".to_string()];
    let mut acc = String::new();
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        acc.push('/');
        acc.push_str(seg);
        out.push(acc.clone());
    }
    out
}

/// The entry of `folder` that lies on the way to `current`, if any:
/// ("/a", "/a/b/c") -> "/a/b".
pub fn child_on_path(folder: &str, current: &str) -> Option<String> {
    if folder == current {
        return None;
    }
    let prefix = if folder == "/" { "/".to_string() } else { format!("{folder}/") };
    let rest = current.strip_prefix(&prefix)?;
    let seg = rest.split('/').next().filter(|s| !s.is_empty())?;
    Some(format!("{prefix}{seg}"))
}
