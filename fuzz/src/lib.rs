//! Containers for the fuzz targets to fill with fuzzed content: ZIP archives
//! (Office Open XML, OpenDocument) and compound files (the old binary .doc,
//! .xls and .ppt). With the container built for it, the fuzzer spends its
//! time in the document readers instead of guessing archive layouts.
//!
//! The writers follow the test helpers in `office` (zip::tests::stored,
//! cfb::tests::build).

use arbitrary::Arbitrary;

/// Part names the Office Open XML and OpenDocument readers look for.
const ZIP_NAMES: &[&str] = &[
    "[Content_Types].xml",
    "_rels/.rels",
    "word/document.xml",
    "word/_rels/document.xml.rels",
    "word/styles.xml",
    "word/numbering.xml",
    "xl/workbook.xml",
    "xl/_rels/workbook.xml.rels",
    "xl/sharedStrings.xml",
    "xl/styles.xml",
    "xl/worksheets/sheet1.xml",
    "xl/worksheets/sheet2.xml",
    "ppt/presentation.xml",
    "ppt/_rels/presentation.xml.rels",
    "ppt/slides/slide1.xml",
    "ppt/slides/slide2.xml",
    "mimetype",
    "content.xml",
    "styles.xml",
    "META-INF/manifest.xml",
];

/// Stream names the binary-format readers look for.
const CFB_NAMES: &[&str] = &[
    "WordDocument",
    "1Table",
    "0Table",
    "Data",
    "Workbook",
    "Book",
    "PowerPoint Document",
    "Current User",
    "EncryptionInfo",
    "EncryptedPackage",
];

/// A file in a ZIP archive: one of the known part names, fuzzed content.
#[derive(Arbitrary, Debug)]
pub struct Part {
    name: u8,
    pub data: Vec<u8>,
}

impl Part {
    pub fn name(&self) -> &'static str {
        ZIP_NAMES[usize::from(self.name) % ZIP_NAMES.len()]
    }
}

/// A stream in a compound file: one of the known names, fuzzed content.
#[derive(Arbitrary, Debug)]
pub struct Stream {
    name: u8,
    pub data: Vec<u8>,
}

impl Stream {
    pub fn name(&self) -> &'static str {
        CFB_NAMES[usize::from(self.name) % CFB_NAMES.len()]
    }
}

/// A ZIP archive of uncompressed files.
pub fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let (mut out, mut dir) = (Vec::new(), Vec::new());
    for (name, data) in files {
        let offset = out.len() as u32;
        let header = |sig: &[u8], extra: &[u8]| {
            let mut h = sig.to_vec();
            h.extend_from_slice(extra);
            h.extend_from_slice(&[0; 8]); // flags, method, time, date
            h.extend_from_slice(&[0; 4]); // crc (not checked by the reader)
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(name.len() as u16).to_le_bytes());
            h.extend_from_slice(&[0; 2]); // extra length
            h
        };
        out.extend(header(b"PK\x03\x04", &[20, 0]));
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        dir.extend(header(b"PK\x01\x02", &[20, 0, 20, 0]));
        dir.extend_from_slice(&[0; 10]); // comment length, disk, attributes
        dir.extend_from_slice(&offset.to_le_bytes());
        dir.extend_from_slice(name.as_bytes());
    }
    let at = out.len() as u32;
    out.extend_from_slice(&dir);
    out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    out.extend_from_slice(&(files.len() as u16).to_le_bytes().repeat(2));
    out.extend_from_slice(&(dir.len() as u32).to_le_bytes());
    out.extend_from_slice(&at.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const NONE: u32 = 0xFFFF_FFFF;
const END: u32 = 0xFFFF_FFFE;
const STREAM: u8 = 2;

/// A compound file with these streams (each in its own sectors; small ones
/// in the mini stream). None if they need more than the one allocation-table
/// sector this writer uses.
pub fn cfb(streams: &[(&str, &[u8])]) -> Option<Vec<u8>> {
    const SEC: usize = 512;
    let mut sectors: Vec<Vec<u8>> = Vec::new();
    let mut fat: Vec<u32> = Vec::new();
    let add_chain = |data: &[u8], sectors: &mut Vec<Vec<u8>>, fat: &mut Vec<u32>| -> u32 {
        let start = sectors.len() as u32;
        let chunks: Vec<&[u8]> = data.chunks(SEC).collect();
        for (i, c) in chunks.iter().enumerate() {
            let mut s = c.to_vec();
            s.resize(SEC, 0);
            sectors.push(s);
            fat.push(if i + 1 == chunks.len() { END } else { start + i as u32 + 1 });
        }
        if chunks.is_empty() {
            END
        } else {
            start
        }
    };
    // Mini stream: streams under 4096 bytes, in 64-byte blocks.
    let (mut mini, mut mini_fat, mut starts) = (Vec::new(), Vec::new(), Vec::new());
    for (_, data) in streams {
        if data.len() < 4096 {
            let start = (mini.len() / 64) as u32;
            let blocks = data.len().div_ceil(64).max(1);
            for i in 0..blocks {
                mini_fat.push(if i + 1 == blocks { END } else { start + i as u32 + 1 });
            }
            let at = mini.len();
            mini.extend_from_slice(data);
            mini.resize(at + blocks * 64, 0);
            starts.push(start);
        } else {
            starts.push(add_chain(data, &mut sectors, &mut fat));
        }
    }
    let mini_start = add_chain(&mini, &mut sectors, &mut fat);
    let mini_fat_bytes: Vec<u8> = mini_fat.iter().flat_map(|v: &u32| v.to_le_bytes()).collect();
    let mini_fat_start = add_chain(&mini_fat_bytes, &mut sectors, &mut fat);
    // Directory: root, then the streams as a chain of right siblings.
    let mut dir = Vec::new();
    let mut dir_entry = |name: &str, kind: u8, right: u32, child: u32, start: u32, size: usize| {
        let mut e = vec![0u8; 128];
        let units: Vec<u8> = name.encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes()).collect();
        e[..units.len()].copy_from_slice(&units);
        e[0x40..0x42].copy_from_slice(&(units.len() as u16).to_le_bytes());
        e[0x42] = kind;
        e[0x44..0x48].copy_from_slice(&NONE.to_le_bytes());
        e[0x48..0x4C].copy_from_slice(&right.to_le_bytes());
        e[0x4C..0x50].copy_from_slice(&child.to_le_bytes());
        e[0x74..0x78].copy_from_slice(&start.to_le_bytes());
        e[0x78..0x80].copy_from_slice(&(size as u64).to_le_bytes());
        dir.extend(e);
    };
    dir_entry("Root Entry", 5, NONE, if streams.is_empty() { NONE } else { 1 }, mini_start, mini.len());
    for (i, (name, data)) in streams.iter().enumerate() {
        let right = if i + 1 < streams.len() { i as u32 + 2 } else { NONE };
        dir_entry(name, STREAM, right, NONE, starts[i], data.len());
    }
    let dir_start = add_chain(&dir, &mut sectors, &mut fat);
    // The allocation table itself, in one sector.
    let fat_sector = sectors.len() as u32;
    fat.push(0xFFFF_FFFD);
    if fat.len() > SEC / 4 {
        return None;
    }
    let mut fs: Vec<u8> = fat.iter().flat_map(|v| v.to_le_bytes()).collect();
    fs.resize(SEC, 0xFF);
    sectors.push(fs);
    let mut h = vec![0u8; 512];
    h[..8].copy_from_slice(&SIGNATURE);
    h[0x18] = 0x3E; // minor version
    h[0x1A] = 3; // major version
    h[0x1C..0x1E].copy_from_slice(&[0xFE, 0xFF]); // byte order
    h[0x1E] = 9; // 512-byte sectors
    h[0x20] = 6; // 64-byte mini sectors
    h[0x2C..0x30].copy_from_slice(&1u32.to_le_bytes());
    h[0x30..0x34].copy_from_slice(&dir_start.to_le_bytes());
    h[0x38..0x3C].copy_from_slice(&4096u32.to_le_bytes());
    h[0x3C..0x40].copy_from_slice(&mini_fat_start.to_le_bytes());
    h[0x40..0x44].copy_from_slice(&1u32.to_le_bytes());
    h[0x44..0x48].copy_from_slice(&END.to_le_bytes());
    for i in 0..109 {
        h[0x4C + i * 4..0x50 + i * 4].copy_from_slice(&if i == 0 { fat_sector } else { NONE }.to_le_bytes());
    }
    h.extend(sectors.concat());
    Some(h)
}
