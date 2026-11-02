//! Previews of office documents: the text of word-processing documents,
//! the cells of spreadsheets and the text of presentations. Reads Office
//! Open XML (docx, xlsx, pptx), OpenDocument (odt, ods, odp) and the older
//! binary Office formats (doc, xls, ppt).
//!
//! The files are untrusted, so nothing in them is believed: every read is
//! bounds-checked, every loop is bounded, and decompression stops at a
//! limit (a small "zip bomb" can't fill memory). Damaged input gives an
//! error or a partial preview, never a panic.

mod binary;
mod cfb;
mod numfmt;
mod odf;
mod ooxml;
mod xml;
mod zip;

/// Most rows and columns kept of each sheet.
pub const MAX_ROWS: usize = 500;
pub const MAX_COLS: usize = 40;
/// Most sheets and slides kept.
pub const MAX_SHEETS: usize = 30;
pub const MAX_SLIDES: usize = 300;
/// Most paragraphs (and tables) kept of a document.
const MAX_BLOCKS: usize = 3000;
/// Most characters kept of a document's text.
const MAX_CHARS: usize = 400_000;

/// What a preview shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Preview {
    /// A word-processing document. `cut`: only its beginning is kept.
    Document { blocks: Vec<Block>, cut: bool },
    /// A spreadsheet.
    Sheets(Vec<Sheet>),
    /// A presentation. `cut`: only its first slides are kept.
    Slides { slides: Vec<Slide>, cut: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    /// A heading, level 1 (largest) to 6.
    Heading(u8, Vec<Span>),
    Paragraph(Vec<Span>),
    /// A list item at a nesting level (0 = top).
    ListItem(u8, Vec<Span>),
    /// Rows of cell texts.
    Table(Vec<Vec<String>>),
}

/// A piece of text with one formatting.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
    pub name: String,
    /// Cell texts from A1, every row equally long; at most
    /// `MAX_ROWS` × `MAX_COLS`.
    pub rows: Vec<Vec<String>>,
    /// The sheet has cells beyond what is kept.
    pub cut: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Slide {
    pub title: String,
    /// The other text on the slide, one paragraph per line.
    pub lines: Vec<String>,
}

/// Why a file can't be previewed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not a format read here, or damaged.
    Unsupported,
    Encrypted,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Error::Unsupported => "No preview available for this file",
            Error::Encrypted => "This document is password-protected",
        })
    }
}

impl std::error::Error for Error {}

/// Whether files with this extension (lower case, no dot) are office
/// documents that `parse` may read.
pub fn handles(ext: &str) -> bool {
    matches!(
        ext,
        "docx" | "docm" | "dotx" | "dotm" | "doc" | "dot" | "odt" | "ott"
            | "xlsx" | "xlsm" | "xltx" | "xltm" | "xls" | "xlt" | "ods" | "ots"
            | "pptx" | "pptm" | "potx" | "potm" | "ppsx" | "ppsm" | "ppt" | "pps" | "pot" | "odp" | "otp"
    )
}

/// Read a document. The format is recognised by the content, not the name.
pub fn parse(data: &[u8]) -> Result<Preview, Error> {
    if data.starts_with(b"PK") {
        let zip = zip::Zip::open(data).ok_or(Error::Unsupported)?;
        return ooxml::parse(&zip).or_else(|| odf::parse(&zip)).unwrap_or(Err(Error::Unsupported));
    }
    if data.starts_with(&cfb::SIGNATURE) {
        return binary::parse(&cfb::Cfb::open(data).ok_or(Error::Unsupported)?);
    }
    Err(Error::Unsupported)
}

/// Formatting of a run of text.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Fmt {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// Add text to a paragraph, joining it to the last span if formatted alike.
pub(crate) fn add(spans: &mut Vec<Span>, text: &str, f: Fmt) {
    if text.is_empty() {
        return;
    }
    match spans.last_mut() {
        Some(s) if (s.bold, s.italic, s.underline) == (f.bold, f.italic, f.underline) => s.text.push_str(text),
        _ => spans.push(Span { text: text.to_string(), bold: f.bold, italic: f.italic, underline: f.underline }),
    }
}

pub(crate) fn plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

/// A document's blocks, collected within the limits.
#[derive(Default)]
pub(crate) struct Doc {
    blocks: Vec<Block>,
    chars: usize,
    cut: bool,
}

impl Doc {
    /// Add a block. Returns false once the limits are reached: stop reading.
    pub fn push(&mut self, block: Block) -> bool {
        if self.blocks.len() >= MAX_BLOCKS || self.chars >= MAX_CHARS {
            self.cut = true;
            return false;
        }
        let len: usize = match &block {
            Block::Heading(_, s) | Block::Paragraph(s) | Block::ListItem(_, s) => s.iter().map(|s| s.text.len()).sum(),
            Block::Table(rows) => rows.iter().flatten().map(String::len).sum(),
        };
        if len == 0 && !matches!(block, Block::Table(_)) {
            // Empty paragraphs only space things out: none at the start,
            // never two in a row.
            let after_space = match self.blocks.last() {
                None => true,
                Some(Block::Paragraph(s)) => s.is_empty(),
                Some(_) => false,
            };
            if !after_space {
                self.blocks.push(Block::Paragraph(Vec::new()));
            }
            return true;
        }
        self.chars += len;
        self.blocks.push(block);
        true
    }

    pub fn finish(mut self) -> Preview {
        while matches!(self.blocks.last(), Some(Block::Paragraph(s)) if s.is_empty()) {
            self.blocks.pop();
        }
        Preview::Document { blocks: self.blocks, cut: self.cut }
    }
}

/// A sheet's cells, collected within `MAX_ROWS` × `MAX_COLS`.
#[derive(Default)]
pub(crate) struct Grid {
    rows: Vec<Vec<String>>,
    pub cut: bool,
}

impl Grid {
    pub fn set(&mut self, row: usize, col: usize, text: String) {
        if text.is_empty() {
            return;
        }
        if row >= MAX_ROWS || col >= MAX_COLS {
            self.cut = true;
            return;
        }
        if self.rows.len() <= row {
            self.rows.resize_with(row + 1, Vec::new);
        }
        let r = &mut self.rows[row];
        if r.len() <= col {
            r.resize(col + 1, String::new());
        }
        r[col] = text;
    }

    pub fn into_sheet(self, name: String) -> Sheet {
        let width = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        let rows = self
            .rows
            .into_iter()
            .map(|mut r| {
                r.resize(width, String::new());
                r
            })
            .collect();
        Sheet { name, rows, cut: self.cut }
    }
}

/// Little-endian reads that fail instead of panicking past the end.
pub(crate) fn u16le(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

pub(crate) fn u32le(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

pub(crate) fn u64le(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// UTF-16LE text (an odd last byte is dropped).
pub(crate) fn utf16(b: &[u8]) -> String {
    let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    String::from_utf16_lossy(&units)
}

pub(crate) fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| char::from(c)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(t: &str) -> Block {
        Block::Paragraph(vec![Span { text: t.into(), ..Default::default() }])
    }

    #[test]
    fn empty_paragraphs_are_collapsed() {
        let mut d = Doc::default();
        for b in [para(""), para("a"), para(""), para(""), para("b"), para("")] {
            d.push(b);
        }
        assert_eq!(d.finish(), Preview::Document { blocks: vec![para("a"), Block::Paragraph(vec![]), para("b")], cut: false });
    }

    #[test]
    fn grids_are_bounded_and_square() {
        let mut g = Grid::default();
        g.set(0, 2, "x".into());
        g.set(1, 0, "y".into());
        g.set(MAX_ROWS, 0, "far".into());
        let s = g.into_sheet("S".into());
        assert_eq!(s.rows, vec![vec!["", "", "x"], vec!["y", "", ""]]);
        assert!(s.cut);
    }

    #[test]
    fn garbage_is_refused_without_panicking() {
        for data in [&b""[..], b"PK", b"PK\x03\x04garbage", &cfb::SIGNATURE, b"%PDF-1.4"] {
            assert_eq!(parse(data), Err(Error::Unsupported));
        }
        let mut cfb_ish = cfb::SIGNATURE.to_vec();
        cfb_ish.resize(4096, 0xFF);
        assert!(parse(&cfb_ish).is_err());
    }
}

/// Prints what every file in `$OFFICE_SAMPLES` gives, for checking real
/// documents by eye: `OFFICE_SAMPLES=dir cargo test -p narwhal-office
/// samples -- --ignored --nocapture`.
#[cfg(test)]
#[test]
#[ignore]
fn samples() {
    let dir = std::env::var("OFFICE_SAMPLES").expect("set OFFICE_SAMPLES to a folder of documents");
    let mut paths: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let data = std::fs::read(&path).unwrap();
        let start = std::time::Instant::now();
        let res = parse(&data);
        println!("=== {} ({} bytes, {:?})", path.display(), data.len(), start.elapsed());
        match res {
            Err(e) => println!("  error: {e}"),
            Ok(Preview::Document { blocks, cut }) => {
                println!("  document: {} blocks, cut {cut}", blocks.len());
                for b in blocks.iter().take(14) {
                    let s = format!("{b:?}");
                    println!("  {}", s.chars().take(160).collect::<String>());
                }
            }
            Ok(Preview::Sheets(sheets)) => {
                for s in &sheets {
                    println!("  sheet {:?}: {} rows x {}, cut {}", s.name, s.rows.len(), s.rows.first().map_or(0, Vec::len), s.cut);
                    for r in s.rows.iter().take(6) {
                        println!("    {:?}", r.iter().take(8).collect::<Vec<_>>());
                    }
                }
            }
            Ok(Preview::Slides { slides, cut }) => {
                println!("  slides: {}, cut {cut}", slides.len());
                for s in slides.iter().take(6) {
                    println!("    {:?} {:?}", s.title, s.lines);
                }
            }
        }
    }
}
