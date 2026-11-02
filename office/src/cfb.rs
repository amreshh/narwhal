//! Compound files ("OLE2"), the container of the binary Office formats: a
//! small filesystem of named streams inside one file, stored in sectors
//! chained through an allocation table.

use crate::{u16le, u32le, u64le, utf16};

pub const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// Sector numbers from here up are markers (end of chain, free, ...).
const LAST_SECTOR: u32 = 0xFFFF_FFFA;
/// No directory entry.
const NONE: u32 = 0xFFFF_FFFF;
const STREAM: u8 = 2;

struct Entry {
    name: String,
    kind: u8,
    left: u32,
    right: u32,
    child: u32,
    start: u32,
    size: u64,
}

pub struct Cfb<'a> {
    data: &'a [u8],
    sector: usize,
    fat: Vec<u32>,
    mini_fat: Vec<u32>,
    mini_stream: Vec<u8>,
    mini_cutoff: u64,
    dir: Vec<Entry>,
}

/// Sector `n` (the header is the sector before sector 0). The file's last
/// sector may be cut short.
fn sector(data: &[u8], size: usize, n: u32) -> Option<&[u8]> {
    let start = (n as usize).checked_add(1)?.checked_mul(size)?;
    data.get(start..start.checked_add(size)?.min(data.len()))
}

/// Follow a chain of `size`-byte blocks through `table`, reading each with
/// `block`. Stops at `max` bytes; a chain that loops is refused.
fn chain<'d>(table: &[u32], start: u32, max: usize, block: impl Fn(u32) -> Option<&'d [u8]>) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut n = start;
    let mut steps = 0;
    while n < LAST_SECTOR && out.len() < max {
        steps += 1;
        if steps > table.len() {
            return None;
        }
        out.extend_from_slice(block(n)?);
        n = *table.get(n as usize)?;
    }
    out.truncate(max);
    Some(out)
}

impl<'a> Cfb<'a> {
    pub fn open(data: &'a [u8]) -> Option<Cfb<'a>> {
        if data.get(..8)? != SIGNATURE {
            return None;
        }
        let shift = u16le(data, 0x1E)?;
        if shift != 9 && shift != 12 || u16le(data, 0x20)? != 6 {
            return None;
        }
        let size = 1usize << shift;
        let sectors = data.len() / size;
        // The sectors of the allocation table: 109 listed in the header,
        // the rest in a chain of DIFAT sectors.
        let mut fat_sectors: Vec<u32> = (0..109).filter_map(|i| u32le(data, 0x4C + i * 4)).filter(|&s| s < LAST_SECTOR).collect();
        let mut difat = u32le(data, 0x44)?;
        for _ in 0..sectors {
            if difat >= LAST_SECTOR {
                break;
            }
            let s = sector(data, size, difat)?;
            let n = s.len() / 4;
            fat_sectors.extend((0..n.saturating_sub(1)).filter_map(|i| u32le(s, i * 4)).filter(|&v| v < LAST_SECTOR));
            difat = u32le(s, (n.saturating_sub(1)) * 4)?;
        }
        fat_sectors.truncate(sectors);
        let mut fat = Vec::new();
        for s in fat_sectors {
            let s = sector(data, size, s)?;
            fat.extend(s.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)));
        }
        let read = |start: u32| chain(&fat, start, data.len(), |n| sector(data, size, n));
        let dir: Vec<Entry> = read(u32le(data, 0x30)?)?.as_chunks::<128>().0.iter().filter_map(|e| entry(e)).collect();
        let root = dir.first()?;
        let mini_fat = read(u32le(data, 0x3C)?)
            .unwrap_or_default()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        let mini_stream = read(root.start).unwrap_or_default();
        Some(Cfb { data, sector: size, fat, mini_fat, mini_stream, mini_cutoff: u64::from(u32le(data, 0x38)?), dir })
    }

    /// A stream in the top folder of the file (not in a sub-storage, where
    /// embedded objects keep streams of the same names), by name.
    pub fn stream(&self, name: &str) -> Option<Vec<u8>> {
        let e = self.top_level().find(|e| e.kind == STREAM && e.name.eq_ignore_ascii_case(name))?;
        let size = usize::try_from(e.size).ok()?;
        if e.size < self.mini_cutoff {
            let mini = |n: u32| {
                let at = (n as usize).checked_mul(64)?;
                self.mini_stream.get(at..at.checked_add(64)?.min(self.mini_stream.len()))
            };
            chain(&self.mini_fat, e.start, size, mini)
        } else {
            chain(&self.fat, e.start, size, |n| sector(self.data, self.sector, n))
        }
    }

    pub fn has(&self, name: &str) -> bool {
        self.top_level().any(|e| e.name.eq_ignore_ascii_case(name))
    }

    /// The entries of the root storage: a binary tree, walked with a bound
    /// (a damaged file could make it loop).
    fn top_level(&self) -> impl Iterator<Item = &Entry> {
        let mut todo = vec![self.dir.first().map_or(NONE, |r| r.child)];
        let mut budget = self.dir.len();
        std::iter::from_fn(move || {
            while let Some(i) = todo.pop() {
                if i == NONE || budget == 0 {
                    continue;
                }
                budget -= 1;
                if let Some(e) = self.dir.get(i as usize) {
                    todo.push(e.left);
                    todo.push(e.right);
                    return Some(e);
                }
            }
            None
        })
    }
}

fn entry(b: &[u8]) -> Option<Entry> {
    let name_len = usize::from(u16le(b, 0x40)?).min(64);
    Some(Entry {
        // UTF-16, with a terminating NUL counted in the length.
        name: utf16(b.get(..name_len.saturating_sub(2))?),
        kind: *b.get(0x42)?,
        left: u32le(b, 0x44)?,
        right: u32le(b, 0x48)?,
        child: u32le(b, 0x4C)?,
        start: u32le(b, 0x74)?,
        // Version 3 files (512-byte sectors) only use the low 32 bits.
        size: u64le(b, 0x78)? & 0xFFFF_FFFF,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A compound file with these streams (each in its own sectors; small
    /// ones go in the mini stream), for tests.
    pub fn build(streams: &[(&str, &[u8])]) -> Vec<u8> {
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
                fat.push(if i + 1 == chunks.len() { 0xFFFF_FFFE } else { start + i as u32 + 1 });
            }
            if chunks.is_empty() { 0xFFFF_FFFE } else { start }
        };
        // Mini stream: streams under 4096 bytes, in 64-byte blocks.
        let (mut mini, mut mini_fat, mut starts) = (Vec::new(), Vec::new(), Vec::new());
        for (_, data) in streams {
            if data.len() < 4096 {
                let start = (mini.len() / 64) as u32;
                let blocks = data.len().div_ceil(64).max(1);
                for i in 0..blocks {
                    mini_fat.push(if i + 1 == blocks { 0xFFFF_FFFE } else { start + i as u32 + 1 });
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
        assert!(fat.len() <= SEC / 4, "test file too big");
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
        h[0x44..0x48].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
        for i in 0..109 {
            h[0x4C + i * 4..0x50 + i * 4].copy_from_slice(&if i == 0 { fat_sector } else { NONE }.to_le_bytes());
        }
        h.extend(sectors.concat());
        h
    }

    #[test]
    fn streams_are_read_from_sectors_and_the_mini_stream() {
        let big: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
        let f = build(&[("Small", b"hello world"), ("Big", &big)]);
        let cfb = Cfb::open(&f).unwrap();
        assert_eq!(cfb.stream("small").unwrap(), b"hello world");
        assert_eq!(cfb.stream("Big").unwrap(), big);
        assert!(cfb.stream("Missing").is_none());
        assert!(cfb.has("Big"));
    }

    #[test]
    fn looping_chains_are_refused() {
        let block = [1u8, 2, 3, 4];
        assert_eq!(chain(&[1, 0], 0, usize::MAX, |_| Some(&block[..])), None, "0 -> 1 -> 0 -> ...");
        assert_eq!(chain(&[1, 0xFFFF_FFFE], 0, usize::MAX, |_| Some(&block[..])).unwrap().len(), 8);
        // A damaged file: every sector number points past the end.
        let mut f = build(&[("Big", &[7u8; 5000])]);
        let n = f.len();
        f.truncate(n - 3000);
        let _ = Cfb::open(&f).and_then(|c| c.stream("Big"));
    }
}
