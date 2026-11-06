//! Reading parts out of a ZIP archive (Office Open XML and OpenDocument
//! files are ZIP archives of XML).

use crate::{u16le, u32le, u64le};

/// Most bytes unpacked from one part; a part that is larger is cut off.
pub const MAX_PART: usize = 48 << 20;
/// Most entries read from the archive's directory.
const MAX_ENTRIES: u64 = 20_000;

struct Entry {
    name: String,
    method: u16,
    encrypted: bool,
    comp: usize,
    offset: usize,
}

pub struct Zip<'a> {
    data: &'a [u8],
    entries: Vec<Entry>,
}

impl<'a> Zip<'a> {
    pub fn open(data: &'a [u8]) -> Option<Zip<'a>> {
        // The end of central directory record is within the last 64 KiB.
        let last = data.len().checked_sub(22)?;
        let eocd = (last.saturating_sub(0xFFFF)..=last).rev().find(|&i| data[i..].starts_with(b"PK\x05\x06"))?;
        let mut count = u64::from(u16le(data, eocd + 10)?);
        let mut dir = u64::from(u32le(data, eocd + 16)?);
        if count == 0xFFFF || dir == 0xFFFF_FFFF {
            // ZIP64: the locator just before points to the ZIP64 record.
            let loc = eocd.checked_sub(20)?;
            if data.get(loc..loc + 4)? == b"PK\x06\x07" {
                // Offsets come from the file and can be anything, so adding
                // to them is checked (here and below).
                let rec = usize::try_from(u64le(data, loc + 8)?).ok()?;
                if data.get(rec..rec.checked_add(4)?)? != b"PK\x06\x06" {
                    return None;
                }
                count = u64le(data, rec + 32)?;
                dir = u64le(data, rec + 48)?;
            }
        }
        let mut entries = Vec::new();
        let mut at = usize::try_from(dir).ok()?;
        for _ in 0..count.min(MAX_ENTRIES) {
            if at.checked_add(4).and_then(|end| data.get(at..end)) != Some(b"PK\x01\x02") {
                break;
            }
            let flags = u16le(data, at + 8)?;
            let method = u16le(data, at + 10)?;
            let mut comp = u64::from(u32le(data, at + 20)?);
            let mut size = u64::from(u32le(data, at + 24)?);
            let name_len = usize::from(u16le(data, at + 28)?);
            let extra_len = usize::from(u16le(data, at + 30)?);
            let comment_len = usize::from(u16le(data, at + 32)?);
            let mut offset = u64::from(u32le(data, at + 42)?);
            let name_at = at + 46;
            let name = String::from_utf8_lossy(data.get(name_at..name_at + name_len)?).into_owned();
            // ZIP64 extra field: 64-bit values for the fields set to 0xFFFFFFFF.
            let extra = data.get(name_at + name_len..name_at + name_len + extra_len)?;
            let mut e = 0;
            while let (Some(id), Some(len)) = (u16le(extra, e), u16le(extra, e + 2)) {
                if id == 1 {
                    let mut f = e + 4;
                    for v in [&mut size, &mut comp, &mut offset] {
                        if *v == 0xFFFF_FFFF {
                            *v = u64le(extra, f)?;
                            f += 8;
                        }
                    }
                }
                e += 4 + usize::from(len);
            }
            let (Ok(comp), Ok(offset)) = (usize::try_from(comp), usize::try_from(offset)) else { break };
            entries.push(Entry { name, method, encrypted: flags & 1 != 0, comp, offset });
            at = name_at + name_len + extra_len + comment_len;
        }
        Some(Zip { data, entries })
    }

    fn entry(&self, name: &str) -> Option<&Entry> {
        // Part names are case-insensitive (Open Packaging Conventions).
        self.entries.iter().find(|e| e.name.eq_ignore_ascii_case(name))
    }

    pub fn has(&self, name: &str) -> bool {
        self.entry(name).is_some()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    /// A part's content, up to `limit` bytes; true if it was cut off there
    /// (or is damaged, and only its beginning could be read).
    pub fn read(&self, name: &str, limit: usize) -> Option<(Vec<u8>, bool)> {
        let e = self.entry(name)?;
        if e.encrypted || self.data.get(e.offset..e.offset.checked_add(4)?)? != b"PK\x03\x04" {
            return None;
        }
        let name_len = usize::from(u16le(self.data, e.offset + 26)?);
        let extra_len = usize::from(u16le(self.data, e.offset + 28)?);
        let start = e.offset + 30 + name_len + extra_len;
        let raw = self.data.get(start..start.checked_add(e.comp)?.min(self.data.len()))?;
        match e.method {
            0 => Some((raw[..raw.len().min(limit)].to_vec(), raw.len() > limit)),
            8 => match miniz_oxide::inflate::decompress_to_vec_with_limit(raw, limit) {
                Ok(v) => Some((v, false)),
                Err(err) => (!err.output.is_empty()).then_some((err.output, true)),
            },
            _ => None,
        }
    }

    /// A part as text (XML parts are UTF-8, sometimes UTF-16).
    pub fn text(&self, name: &str) -> Option<(String, bool)> {
        let (bytes, cut) = self.read(name, MAX_PART)?;
        let text = match bytes.as_slice() {
            [0xFF, 0xFE, rest @ ..] => crate::utf16(rest),
            [0xFE, 0xFF, rest @ ..] => {
                let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
                String::from_utf16_lossy(&units)
            }
            [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
            b => String::from_utf8_lossy(b).into_owned(),
        };
        Some((text, cut))
    }
}

/// The part that `target` (relative to the folder `base`, or absolute)
/// names: `join("xl", "worksheets/sheet1.xml")` -> `xl/worksheets/sheet1.xml`.
pub fn join(base: &str, target: &str) -> String {
    let (mut parts, target) = match target.strip_prefix('/') {
        Some(abs) => (Vec::new(), abs),
        None => (base.split('/').filter(|s| !s.is_empty()).collect::<Vec<_>>(), target),
    };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A ZIP archive of uncompressed files, for tests.
    pub fn stored(files: &[(&str, &[u8])]) -> Vec<u8> {
        let (mut out, mut dir) = (Vec::new(), Vec::new());
        for (name, data) in files {
            let offset = out.len() as u32;
            let header = |sig: &[u8], extra: &[u8]| {
                let mut h = sig.to_vec();
                h.extend_from_slice(extra);
                h.extend_from_slice(&[0; 8]); // flags, method, time, date
                h.extend_from_slice(&[0; 4]); // crc (unchecked)
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

    #[test]
    fn stored_parts_are_read() {
        let z = stored(&[("a/b.xml", b"<x/>"), ("[Content_Types].xml", b"ct")]);
        let zip = Zip::open(&z).unwrap();
        assert_eq!(zip.read("A/B.XML", 100), Some((b"<x/>".to_vec(), false)));
        assert_eq!(zip.read("a/b.xml", 2), Some((b"<x".to_vec(), true)));
        assert!(zip.read("missing", 10).is_none());
        assert_eq!(zip.names().count(), 2);
    }

    #[test]
    fn offsets_that_would_overflow_are_refused() {
        // Found by fuzzing: ZIP64 records pointing near the end of the address space.
        let found: [&[u8]; 2] = [
            b"PK\x01\x02PK\x05\x06\x4b\xb1\x1a\xe1\xfc\xff\xff\xff\x00PK\x06\x07\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\
              PK\x01\x02PK\x05\x06\x4b\xb1\x1a\xe1\xfc\xff\xff\xff\x00PK\x06\x07\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\
              \xff\xff\xff\xff\x24\xff\x00\x00",
            b"PK\x21\x00\x01PK\x06\x07PKPS\xfc\xff\xff\xff\xff\xff\xff\xff\xbb\x4b\x09\xfbPK\x05\x06\xd3\xff\x4e\x00\
              \xff\xff\xff\xff\x18\x00\x00\x00\x00\x00\x00\x00\x00\x02",
        ];
        for data in found {
            assert!(crate::parse(data).is_err());
        }
        // A ZIP64 extra field that moves the entry to offset u64::MAX.
        let mut data = stored(&[("a.xml", b"x")]);
        let dir = 30 + 5 + 1; // after the local header, the name and the content
        data[dir + 30..dir + 32].copy_from_slice(&12u16.to_le_bytes()); // extra field length
        data[dir + 42..dir + 46].copy_from_slice(&[0xFF; 4]); // offset: in the extra field
        let name_end = dir + 46 + 5;
        data.splice(name_end..name_end, [1, 0, 8, 0].into_iter().chain([0xFF; 8]));
        let zip = Zip::open(&data).unwrap();
        assert!(zip.has("a.xml"));
        assert!(zip.read("a.xml", 100).is_none());
    }

    #[test]
    fn deflated_parts_stop_at_the_limit() {
        // A deflate stream of a million zeros: the kind of part a zip bomb has.
        let big = miniz_oxide::deflate::compress_to_vec(&vec![0u8; 1_000_000], 6);
        let mut z = stored(&[("big", &big)]);
        // Mark it deflated (method 8) in both headers.
        let local = 0;
        let central = z.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
        z[local + 8] = 8;
        z[central + 10] = 8;
        let zip = Zip::open(&z).unwrap();
        let (data, cut) = zip.read("big", 1000).unwrap();
        assert_eq!((data.len(), cut), (1000, true));
        assert_eq!(zip.read("big", 2_000_000).unwrap().0.len(), 1_000_000);
    }

    #[test]
    fn paths() {
        assert_eq!(join("xl", "worksheets/sheet1.xml"), "xl/worksheets/sheet1.xml");
        assert_eq!(join("xl", "/xl/sharedStrings.xml"), "xl/sharedStrings.xml");
        assert_eq!(join("ppt/slides", "../media/a.png"), "ppt/media/a.png");
        assert_eq!(join("", "word/document.xml"), "word/document.xml");
    }
}
