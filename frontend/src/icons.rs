//! Hand-drawn SVG icons. UI glyphs are 16×16 strokes coloured by
//! `currentColor`; file icons are 48×48 and styled through CSS classes.

use crate::util::{cat_color, category, ext, Cat};
use narwhal_shared::Entry;

macro_rules! glyph {
    ($name:ident, $body:expr) => {
        pub const $name: &str = concat!(r#"<svg viewBox="0 0 16 16" aria-hidden="true">"#, $body, "</svg>");
    };
}

glyph!(BACK, r#"<path d="M10 3.5 5.5 8l4.5 4.5"/>"#);
glyph!(FORWARD, r#"<path d="m6 3.5 4.5 4.5L6 12.5"/>"#);
glyph!(UP, r#"<path d="M8 13V3.5M3.5 8 8 3.5 12.5 8"/>"#);
glyph!(CHEVRON, r#"<path d="m6.5 4.5 3.5 3.5-3.5 3.5"/>"#);
glyph!(VIEW_ICONS, r#"<rect x="2.5" y="2.5" width="4.5" height="4.5" rx=".8"/><rect x="9" y="2.5" width="4.5" height="4.5" rx=".8"/><rect x="2.5" y="9" width="4.5" height="4.5" rx=".8"/><rect x="9" y="9" width="4.5" height="4.5" rx=".8"/>"#);
glyph!(VIEW_COMPACT, r#"<rect x="2" y="3" width="3" height="3" rx=".5"/><rect x="2" y="10" width="3" height="3" rx=".5"/><path d="M7 4.5h7M7 11.5h7"/>"#);
glyph!(VIEW_COLUMNS, r#"<rect x="1.5" y="2.5" width="13" height="11" rx="1.5"/><path d="M5.8 2.5v11M10.2 2.5v11"/>"#);
glyph!(VIEW_DETAILS, r#"<path d="M2.5 3.5h11M2.5 6.5h11M2.5 9.5h11M2.5 12.5h11"/>"#);
glyph!(SPLIT, r#"<rect x="2" y="2.5" width="12" height="11" rx="1.5"/><path d="M8 2.5v11"/>"#);
glyph!(SEARCH, r#"<circle cx="7" cy="7" r="4"/><path d="m10 10 3.5 3.5"/>"#);
glyph!(FILTER, r#"<path d="M2.5 3.5h11l-4.2 5v4l-2.6 1.2V8.5z"/>"#);
glyph!(MENU, r#"<path d="M2.5 4h11M2.5 8h11M2.5 12h11"/>"#);
glyph!(NEW_FOLDER, r#"<path d="M2 4.5c0-.6.4-1 1-1h3l1.5 1.5H13c.6 0 1 .4 1 1v6.5c0 .6-.4 1-1 1H3c-.6 0-1-.4-1-1z"/><path d="M8 7.5v4M6 9.5h4"/>"#);
glyph!(NEW_FILE, r#"<path d="M4 1.5h5.5l3 3V14c0 .3-.2.5-.5.5H4c-.3 0-.5-.2-.5-.5V2c0-.3.2-.5.5-.5z"/><path d="M8 7v5M5.5 9.5h5"/>"#);
glyph!(UPLOAD, r#"<path d="M8 10.5V2.5M4.5 6 8 2.5 11.5 6M2.5 10v2.5c0 .6.4 1 1 1h9c.6 0 1-.4 1-1V10"/>"#);
glyph!(DOWNLOAD, r#"<path d="M8 2.5v8M4.5 7 8 10.5 11.5 7M2.5 10v2.5c0 .6.4 1 1 1h9c.6 0 1-.4 1-1V10"/>"#);
glyph!(CLOSE, r#"<path d="m4 4 8 8M12 4l-8 8"/>"#);
glyph!(PREVIEW, r#"<rect x="2" y="3" width="12" height="10" rx="1.5"/><circle cx="5.5" cy="6.5" r="1.2"/><path d="m2.5 12 4-4 3 3 1.5-1.5 2.5 2.5"/>"#);
glyph!(DRIVE, r#"<rect x="1.5" y="4.5" width="13" height="7" rx="1.5"/><path d="M1.5 9h13"/><circle cx="12" cy="10.3" r=".3"/>"#);
glyph!(FOLDER, r#"<path d="M2 4.5c0-.6.4-1 1-1h3l1.5 1.5H13c.6 0 1 .4 1 1v6.5c0 .6-.4 1-1 1H3c-.6 0-1-.4-1-1z"/>"#);
glyph!(OPEN, r#"<path d="M9.5 2.5h4v4M13.5 2.5 8 8M12 9.5V13c0 .3-.2.5-.5.5h-8A.5.5 0 0 1 3 13V5c0-.3.2-.5.5-.5H7"/>"#);
glyph!(CUT, r#"<circle cx="4.5" cy="11.5" r="2"/><circle cx="11.5" cy="11.5" r="2"/><path d="M5.8 10 12 2.5M10.2 10 4 2.5"/>"#);
glyph!(COPY, r#"<rect x="5" y="5" width="8.5" height="9" rx="1"/><path d="M3 11V3c0-.6.4-1 1-1h6"/>"#);
glyph!(PASTE, r#"<rect x="3" y="3" width="10" height="11" rx="1"/><path d="M6 3V2h4v1M5.5 7h5M5.5 10h3"/>"#);
glyph!(RENAME, r#"<path d="M2.5 13.5h3l7.5-7.5-3-3-7.5 7.5zM8.5 4.5l3 3"/>"#);
glyph!(TRASH, r#"<path d="M2.5 4h11M6 4V2.5h4V4M4 4l.7 9.5h6.6L12 4M6.5 6.5v5M9.5 6.5v5"/>"#);
glyph!(LINK, r#"<path d="M6.5 9.5a3 3 0 0 0 4.2 0l2.1-2.1a3 3 0 0 0-4.2-4.2L7.8 4M9.5 6.5a3 3 0 0 0-4.2 0L3.2 8.6a3 3 0 0 0 4.2 4.2l.8-.8"/>"#);
glyph!(INFO, r#"<circle cx="8" cy="8" r="6"/><path d="M8 7.5v4M8 5v.2"/>"#);
glyph!(SELECT_ALL, r#"<rect x="2.5" y="2.5" width="11" height="11" rx="1.5" stroke-dasharray="2 1.5"/><path d="m5.5 8 2 2 3.5-4"/>"#);
glyph!(REFRESH, r#"<path d="M13 8a5 5 0 1 1-1.5-3.5M13 2.5v3h-3"/>"#);
glyph!(CHECK, r#"<path d="m3.5 8.5 3 3 6-7"/>"#);
glyph!(KEYBOARD, r#"<rect x="1.5" y="4" width="13" height="8" rx="1.5"/><path d="M4 6.5h.1M6.5 6.5h.1M9 6.5h.1M11.5 6.5h.1M5 9.5h6"/>"#);
glyph!(EMPTY, "");

const LINK_BADGE: &str = r##"<g transform="translate(3 31)"><rect class="badge" width="13" height="13" rx="3"/><path d="M4 9.5 9.5 4M6 4h3.5v3.5" fill="none" stroke="#2c3e50" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></g>"##;

/// Full-colour 48×48 icon for a directory entry.
pub fn file_icon(e: &Entry) -> String {
    let cat = category(e);
    let link = if e.symlink { LINK_BADGE } else { "" };
    if cat == Cat::Folder {
        return format!(
            r#"<svg class="fi" viewBox="0 0 48 48" aria-hidden="true"><path class="fb" d="M4 11a3 3 0 0 1 3-3h11.2l4 4H41a3 3 0 0 1 3 3v24a3 3 0 0 1-3 3H7a3 3 0 0 1-3-3z"/><path class="ff" d="M4 18a2 2 0 0 1 2-2h36a2 2 0 0 1 2 2v21a3 3 0 0 1-3 3H7a3 3 0 0 1-3-3z"/><path class="fh" d="M6 17.5h36"/>{link}</svg>"#
        );
    }
    let color = cat_color(cat);
    // Only alphanumerics, so the label can never inject markup.
    let label: String = ext(&e.name)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(4)
        .collect::<String>()
        .to_uppercase();
    let art = match cat {
        Cat::Image => r#"<circle cx="18" cy="15" r="2.6" class="art" /><path d="M13 25l6-6 4 4 3-3 7 5z" class="art"/>"#,
        Cat::Video => r#"<path d="M20 12.5v10l8.5-5z" class="art"/>"#,
        Cat::Audio => r#"<path d="M21 23.5V13l9-2v10" class="art-s"/><circle cx="19" cy="23.5" r="2.3" class="art"/><circle cx="28" cy="21" r="2.3" class="art"/>"#,
        Cat::Archive => r#"<path d="M22 6v20" class="art-s" stroke-dasharray="2 2"/><rect x="20" y="17" width="4" height="5" rx="1" class="art"/>"#,
        Cat::Code => r#"<path d="m18 13-4 4 4 4M26 13l4 4-4 4" class="art-s"/>"#,
        _ => r#"<path d="M14 13h14M14 17h18M14 21h12" class="ln"/>"#,
    };
    let band = if label.is_empty() {
        String::new()
    } else {
        format!(
            r##"<rect x="9" y="30" width="30" height="10" fill="{color}"/><text x="24" y="37.6" text-anchor="middle" font-size="7.5" font-weight="700" fill="#fff" font-family="sans-serif" letter-spacing=".3">{label}</text>"##
        )
    };
    format!(
        r##"<svg class="fi" viewBox="0 0 48 48" aria-hidden="true" style="--c:{color}"><path class="pg" d="M11 4h18l10 10v28a2 2 0 0 1-2 2H11a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z"/><path class="fold" d="M29 4v8a2 2 0 0 0 2 2h8z"/>{art}{band}{link}</svg>"##
    )
}
