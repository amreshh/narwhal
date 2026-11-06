//! The binary Office formats of Office 97–2003: Word (doc), Excel (xls,
//! BIFF8) and PowerPoint (ppt), stored in compound files.

use crate::cfb::Cfb;
use crate::numfmt::{self, Style};
use crate::{
    latin1, u16le, u32le, utf16, Block, Doc, Error, Grid, Preview, Sheet, Slide, Span, MAX_CHARS, MAX_SHEETS,
    MAX_SLIDES,
};
use std::collections::HashMap;

pub fn parse(cfb: &Cfb) -> Result<Preview, Error> {
    // A password-protected docx, xlsx or pptx is a compound file too.
    if cfb.has("EncryptedPackage") {
        return Err(Error::Encrypted);
    }
    if let Some(wb) = cfb.stream("Workbook") {
        return xls(&wb);
    }
    if let Some(wd) = cfb.stream("WordDocument") {
        return doc(cfb, &wd);
    }
    if let Some(pp) = cfb.stream("PowerPoint Document") {
        return ppt(&pp);
    }
    // Excel 5/95 ("Book"), Works, other compound files.
    Err(Error::Unsupported)
}

// ---------------------------------------------------------------------------
// Word
// ---------------------------------------------------------------------------

/// Windows-1252, which Word uses for "compressed" (8-bit) text.
fn cp1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}', '\u{90}', '‘',
        '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9F => HIGH[usize::from(b - 0x80)],
        b => char::from(b),
    }
}

/// The main text of a Word 97–2003 document, read through its piece table
/// (the text is stored in pieces, 8- or 16-bit, in any order).
fn doc_text(cfb: &Cfb, wd: &[u8]) -> Result<String, Error> {
    let bad = || Error::Unsupported;
    if u16le(wd, 0) != Some(0xA5EC) || u16le(wd, 2).ok_or_else(bad)? < 0xC1 {
        return Err(bad()); // Word 6/95 and older
    }
    let flags = u16le(wd, 0x0A).ok_or_else(bad)?;
    if flags & 0x0100 != 0 {
        return Err(Error::Encrypted);
    }
    let table = cfb.stream(if flags & 0x0200 != 0 { "1Table" } else { "0Table" }).ok_or_else(bad)?;
    // The FIB: a block of u16s, a block of u32s, then (offset, size) pairs.
    let csw = usize::from(u16le(wd, 0x20).ok_or_else(bad)?);
    let lw = 0x22 + csw * 2;
    let cslw = usize::from(u16le(wd, lw).ok_or_else(bad)?);
    let ccp_text = u32le(wd, lw + 2 + 3 * 4).ok_or_else(bad)? as usize;
    let pairs = lw + 2 + cslw * 4 + 2;
    let fc_clx = u32le(wd, pairs + 33 * 8).ok_or_else(bad)? as usize;
    let lcb_clx = u32le(wd, pairs + 33 * 8 + 4).ok_or_else(bad)? as usize;
    let clx = table.get(fc_clx..fc_clx.saturating_add(lcb_clx)).ok_or_else(bad)?;
    // Skip formatting (Prc) entries; then the piece table (Pcdt).
    let mut p = 0;
    while clx.get(p) == Some(&1) {
        p += 3 + usize::from(u16le(clx, p + 1).ok_or_else(bad)?);
    }
    if clx.get(p) != Some(&2) {
        return Err(bad());
    }
    let lcb = u32le(clx, p + 1).ok_or_else(bad)? as usize;
    let plc = clx.get(p + 5..(p + 5).saturating_add(lcb)).ok_or_else(bad)?;
    let n = lcb.saturating_sub(4) / 12;
    let mut text = String::new();
    for i in 0..n {
        let (Some(cp0), Some(cp1), Some(fc)) =
            (u32le(plc, i * 4), u32le(plc, i * 4 + 4), u32le(plc, (n + 1) * 4 + i * 8 + 2))
        else {
            break;
        };
        let (cp0, cp1) = (cp0 as usize, (cp1 as usize).min(ccp_text));
        if cp0 >= ccp_text || text.len() > MAX_CHARS * 2 {
            break;
        }
        let len = cp1.saturating_sub(cp0);
        if fc & 0x4000_0000 != 0 {
            let at = ((fc & 0x3FFF_FFFF) / 2) as usize;
            let bytes = wd.get(at..at.saturating_add(len).min(wd.len())).unwrap_or_default();
            text.extend(bytes.iter().map(|&b| cp1252(b)));
        } else {
            let at = fc as usize;
            text.push_str(&utf16(wd.get(at..at.saturating_add(len * 2).min(wd.len())).unwrap_or_default()));
        }
    }
    Ok(text)
}

fn doc(cfb: &Cfb, wd: &[u8]) -> Result<Preview, Error> {
    let text = doc_text(cfb, wd)?;
    let mut doc = Doc::default();
    let mut para = String::new();
    // Fields: code, then (after a separator) the result that is shown.
    let mut fields: Vec<bool> = Vec::new();
    for c in text.chars() {
        match c {
            '\u{13}' => fields.push(false),
            '\u{14}' => {
                if let Some(f) = fields.last_mut() {
                    *f = true;
                }
            }
            '\u{15}' => {
                fields.pop();
            }
            _ if fields.last() == Some(&false) => {}
            // Paragraph, table cell and page ends.
            '\r' | '\u{7}' | '\u{c}' => {
                let spans = if para.is_empty() {
                    Vec::new()
                } else {
                    vec![Span { text: std::mem::take(&mut para), ..Span::default() }]
                };
                if !doc.push(Block::Paragraph(spans)) {
                    break;
                }
            }
            '\u{b}' => para.push('\n'),
            '\u{1e}' => para.push('-'),
            '\t' => para.push('\t'),
            // Pictures, drawings, footnote and comment marks, soft hyphens.
            c if c < ' ' => {}
            c => para.push(c),
        }
    }
    if !para.is_empty() {
        doc.push(Block::Paragraph(vec![Span { text: para, ..Span::default() }]));
    }
    Ok(doc.finish())
}

// ---------------------------------------------------------------------------
// Excel
// ---------------------------------------------------------------------------

/// The records of a BIFF stream: (type, body), from `at`.
struct Records<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Records<'a> {
    fn peek(&self) -> Option<u16> {
        u16le(self.data, self.at)
    }
}

impl<'a> Iterator for Records<'a> {
    type Item = (u16, &'a [u8]);
    fn next(&mut self) -> Option<Self::Item> {
        let kind = u16le(self.data, self.at)?;
        let len = usize::from(u16le(self.data, self.at + 2)?);
        let body = self.data.get(self.at + 4..self.at + 4 + len)?;
        self.at += 4 + len;
        Some((kind, body))
    }
}

/// A string inside one record: `len_bytes` (1 or 2) of length, flags,
/// characters (8- or 16-bit), and formatting to skip.
fn xl_string(b: &[u8], at: usize, len_bytes: usize) -> Option<String> {
    let cch = if len_bytes == 1 { usize::from(*b.get(at)?) } else { usize::from(u16le(b, at)?) };
    let flags = *b.get(at + len_bytes)?;
    let mut p = at + len_bytes + 1;
    if flags & 0x08 != 0 {
        p += 2;
    }
    if flags & 0x04 != 0 {
        p += 4;
    }
    let wide = flags & 0x01 != 0;
    let end = p.saturating_add(if wide { cch * 2 } else { cch }).min(b.len());
    let chars = b.get(p..end)?;
    Some(if wide { utf16(chars) } else { latin1(chars) })
}

/// Bytes of the shared string table, which may continue over several
/// records. A string's characters split by a record boundary resume with a
/// new flags byte (they may switch between 8 and 16 bits).
struct Parts<'a> {
    parts: Vec<&'a [u8]>,
    i: usize,
    at: usize,
}

impl Parts<'_> {
    fn at_part_end(&self) -> bool {
        self.parts.get(self.i).is_some_and(|p| self.at >= p.len())
    }

    fn next_part(&mut self) -> Option<()> {
        self.i += 1;
        self.at = 0;
        (self.i < self.parts.len()).then_some(())
    }

    fn byte(&mut self) -> Option<u8> {
        while self.at_part_end() {
            self.next_part()?;
        }
        let b = *self.parts.get(self.i)?.get(self.at)?;
        self.at += 1;
        Some(b)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes([self.byte()?, self.byte()?]))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes([self.byte()?, self.byte()?, self.byte()?, self.byte()?]))
    }

    fn skip(&mut self, mut n: usize) -> Option<()> {
        while n > 0 {
            while self.at_part_end() {
                self.next_part()?;
            }
            let left = self.parts.get(self.i)?.len() - self.at;
            let k = left.min(n);
            self.at += k;
            n -= k;
        }
        Some(())
    }

    fn string(&mut self) -> Option<String> {
        let cch = usize::from(self.u16()?);
        let mut flags = self.byte()?;
        let runs = if flags & 0x08 != 0 { usize::from(self.u16()?) } else { 0 };
        let ext = if flags & 0x04 != 0 { self.u32()? as usize } else { 0 };
        let mut units = Vec::with_capacity(cch.min(4096));
        for _ in 0..cch {
            if self.at_part_end() {
                self.next_part()?;
                flags = *self.parts.get(self.i)?.first()?;
                self.at = 1;
            }
            let part = self.parts.get(self.i)?;
            if flags & 0x01 != 0 {
                units.push(u16le(part, self.at)?);
                self.at += 2;
            } else {
                units.push(u16::from(*part.get(self.at)?));
                self.at += 1;
            }
        }
        self.skip(runs * 4 + ext)?;
        Some(String::from_utf16_lossy(&units))
    }
}

fn shared_strings(parts: Vec<&[u8]>) -> Vec<String> {
    let mut r = Parts { parts, i: 0, at: 0 };
    let (_, Some(unique)) = (r.u32(), r.u32()) else { return Vec::new() };
    let mut out = Vec::with_capacity((unique as usize).min(65_536));
    for _ in 0..unique.min(2_000_000) {
        match r.string() {
            Some(s) => out.push(s),
            None => break,
        }
    }
    out
}

/// An RK number: a 30-bit integer or the top of a double, maybe /100.
fn rk(v: u32) -> f64 {
    let n = if v & 0x02 != 0 { f64::from((v as i32) >> 2) } else { f64::from_bits(u64::from(v & 0xFFFF_FFFC) << 32) };
    if v & 0x01 != 0 {
        n / 100.0
    } else {
        n
    }
}

fn error_text(code: u8) -> &'static str {
    match code {
        0x00 => "#NULL!",
        0x07 => "#DIV/0!",
        0x0F => "#VALUE!",
        0x17 => "#REF!",
        0x1D => "#NAME?",
        0x24 => "#NUM!",
        0x2A => "#N/A",
        _ => "#ERROR!",
    }
}

fn xls(wb: &[u8]) -> Result<Preview, Error> {
    let mut recs = Records { data: wb, at: 0 };
    match recs.next() {
        // BIFF8 (Excel 97 and later).
        Some((0x0809, bof)) if u16le(bof, 0) == Some(0x0600) => {}
        _ => return Err(Error::Unsupported),
    }
    let (mut sheets, mut formats, mut xf_formats) = (Vec::new(), HashMap::new(), Vec::new());
    let (mut strings, mut date1904) = (Vec::new(), false);
    while let Some((kind, b)) = recs.next() {
        match kind {
            0x002F => return Err(Error::Encrypted), // FILEPASS
            0x0022 => date1904 = u16le(b, 0) == Some(1),
            0x041E => {
                if let (Some(id), Some(code)) = (u16le(b, 0), xl_string(b, 2, 2)) {
                    formats.insert(u32::from(id), code);
                }
            }
            0x00E0 => xf_formats.push(u32::from(u16le(b, 2).unwrap_or(0))),
            // BOUNDSHEET: worksheets (type 0) with the offset of their records.
            0x0085 if b.get(5) == Some(&0) => {
                if let (Some(at), Some(name)) = (u32le(b, 0), xl_string(b, 6, 1)) {
                    sheets.push((name, at as usize));
                }
            }
            0x00FC => {
                let mut parts = vec![b];
                while recs.peek() == Some(0x003C) {
                    parts.extend(recs.next().map(|(_, b)| b));
                }
                strings = shared_strings(parts);
            }
            0x000A => break,
            _ => {}
        }
    }
    let styles: Vec<Style> =
        xf_formats.iter().map(|id| numfmt::style(*id, formats.get(id).map(String::as_str))).collect();
    let out: Vec<Sheet> = sheets
        .into_iter()
        .take(MAX_SHEETS)
        .map(|(name, at)| xls_sheet(wb, at, name, &strings, &styles, date1904))
        .collect();
    if out.is_empty() {
        return Err(Error::Unsupported);
    }
    Ok(Preview::Sheets(out))
}

fn xls_sheet(wb: &[u8], at: usize, name: String, strings: &[String], styles: &[Style], date1904: bool) -> Sheet {
    let mut grid = Grid::default();
    let mut recs = Records { data: wb, at };
    if recs.next().map(|(k, _)| k) != Some(0x0809) {
        return grid.into_sheet(name);
    }
    let number = |xf: u16, v: f64| numfmt::show(v, styles.get(usize::from(xf)).copied().unwrap_or_default(), date1904);
    // A formula whose text result follows in a STRING record.
    let mut pending: Option<(usize, usize)> = None;
    for (kind, b) in recs {
        match kind {
            0x000A => break,
            0x0207 => {
                if let Some((r, c)) = pending.take() {
                    grid.set(r, c, xl_string(b, 0, 2).unwrap_or_default());
                }
                continue;
            }
            _ => {}
        }
        let (Some(row), Some(col)) = (u16le(b, 0).map(usize::from), u16le(b, 2).map(usize::from)) else { continue };
        let xf = u16le(b, 4).unwrap_or(0);
        match kind {
            0x00FD => {
                grid.set(row, col, u32le(b, 6).and_then(|i| strings.get(i as usize)).cloned().unwrap_or_default())
            }
            0x0204 | 0x00D6 => grid.set(row, col, xl_string(b, 6, 2).unwrap_or_default()),
            0x0203 => {
                if let Some(v) = b.get(6..14) {
                    grid.set(row, col, number(xf, f64::from_le_bytes(v.try_into().unwrap_or_default())));
                }
            }
            0x027E => {
                if let Some(v) = u32le(b, 6) {
                    grid.set(row, col, number(xf, rk(v)));
                }
            }
            // MULRK: several RK numbers in a row.
            0x00BD => {
                for k in 0..b.len().saturating_sub(6) / 6 {
                    if let (Some(xf), Some(v)) = (u16le(b, 4 + k * 6), u32le(b, 6 + k * 6)) {
                        grid.set(row, col + k, number(xf, rk(v)));
                    }
                }
            }
            0x0205 => match (b.get(6), b.get(7)) {
                (Some(&e), Some(1)) => grid.set(row, col, error_text(e).into()),
                (Some(&v), Some(_)) => grid.set(row, col, if v != 0 { "TRUE" } else { "FALSE" }.into()),
                _ => {}
            },
            // FORMULA: the last result is kept with it.
            0x0006 => match b.get(6..14) {
                Some(r) if r[6] == 0xFF && r[7] == 0xFF => match r[0] {
                    0 => pending = Some((row, col)),
                    1 => grid.set(row, col, if r[2] != 0 { "TRUE" } else { "FALSE" }.into()),
                    2 => grid.set(row, col, error_text(r[2]).into()),
                    _ => {}
                },
                Some(r) => grid.set(row, col, number(xf, f64::from_le_bytes(r.try_into().unwrap_or_default()))),
                None => {}
            },
            _ => {}
        }
    }
    grid.into_sheet(name)
}

// ---------------------------------------------------------------------------
// PowerPoint
// ---------------------------------------------------------------------------

/// Text of a text atom, in paragraphs.
fn add_text(slide: &mut Slide, text: &str, title: bool) {
    let lines = text.split(['\r', '\u{b}']).map(str::trim).filter(|l| !l.is_empty());
    if title && slide.title.is_empty() {
        slide.title = lines.collect::<Vec<_>>().join(" ");
    } else {
        slide.lines.extend(lines.map(String::from));
    }
}

fn ppt(s: &[u8]) -> Result<Preview, Error> {
    // Records nest: (end, kind) of the open containers.
    let mut open: Vec<(usize, u16, u16)> = Vec::new();
    // Slide text is listed per slide (SlideListWithText); text of other
    // boxes is in the slides' drawings. Both are collected; the list is
    // preferred.
    let (mut listed, mut drawn): (Vec<Slide>, Vec<Slide>) = (Vec::new(), Vec::new());
    let (mut title_listed, mut title_drawn) = (false, false);
    let mut at = 0;
    while let (Some(vi), Some(kind), Some(len)) = (u16le(s, at), u16le(s, at + 2), u32le(s, at + 4)) {
        while open.last().is_some_and(|&(end, ..)| at >= end) {
            open.pop();
        }
        let body_at = at + 8;
        let end = body_at.saturating_add(len as usize).min(s.len());
        if vi & 0x000F == 0x000F {
            // A container: its records follow.
            if kind == 0x03EE {
                drawn.push(Slide::default());
            }
            open.push((end, kind, vi >> 4));
            at = body_at;
            continue;
        }
        let in_list = open.iter().any(|&(_, k, instance)| k == 0x0FF0 && instance == 0);
        let in_slide = open.iter().any(|&(_, k, _)| k == 0x03EE);
        let body = &s[body_at.min(end)..end];
        match kind {
            0x03F3 if in_list => listed.push(Slide::default()),
            // TextHeaderAtom: 0 = title, 6 = centred title.
            0x0F9F => {
                let title = matches!(u32le(body, 0), Some(0 | 6));
                if in_list {
                    title_listed = title;
                } else {
                    title_drawn = title;
                }
            }
            0x0FA0 | 0x0FA8 => {
                let text = if kind == 0x0FA0 { utf16(body) } else { latin1(body) };
                if in_list {
                    if let Some(sl) = listed.last_mut() {
                        add_text(sl, &text, title_listed);
                    }
                } else if in_slide {
                    if let Some(sl) = drawn.last_mut() {
                        add_text(sl, &text, title_drawn);
                    }
                }
            }
            _ => {}
        }
        at = end.max(at + 8);
    }
    let has_text = |v: &[Slide]| v.iter().any(|s| !s.title.is_empty() || !s.lines.is_empty());
    let mut slides = if has_text(&listed) || !has_text(&drawn) { listed } else { drawn };
    let cut = slides.len() > MAX_SLIDES;
    slides.truncate(MAX_SLIDES);
    if slides.is_empty() {
        return Err(Error::Unsupported);
    }
    Ok(Preview::Slides { slides, cut })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfb::tests::build;
    use crate::parse;

    fn record(kind: u16, body: &[u8]) -> Vec<u8> {
        let mut r = kind.to_le_bytes().to_vec();
        r.extend_from_slice(&(body.len() as u16).to_le_bytes());
        r.extend_from_slice(body);
        r
    }

    #[test]
    fn excel_97_workbooks() {
        // Globals: BOF, a date format on XF 1, shared strings split over a
        // CONTINUE record (switching to 16-bit), one sheet; then the sheet.
        let mut sst = 2u32.to_le_bytes().to_vec();
        sst.extend_from_slice(&2u32.to_le_bytes());
        sst.extend_from_slice(&[3, 0, 0]);
        sst.extend_from_slice(b"one");
        sst.extend_from_slice(&[4, 0, 0]);
        sst.extend_from_slice(b"tw");
        let cont = [1u8, b'o', 0, 0x3A, 0x26]; // "o☺" in 16-bit
        let mut globals = record(0x0809, &[0, 6, 5, 0]);
        globals.extend(record(0x00E0, &[0, 0, 0, 0]));
        globals.extend(record(0x00E0, &[0, 0, 14, 0]));
        globals.extend(record(0x00FC, &sst));
        globals.extend(record(0x003C, &cont));
        // After this BOUNDSHEET (4 + 10 bytes) and EOF (4 bytes).
        let sheet_at = globals.len() + 14 + 4;
        let mut bs = (sheet_at as u32).to_le_bytes().to_vec();
        bs.extend_from_slice(&[0, 0, 2, 0]);
        bs.extend_from_slice(b"Up");
        globals.extend(record(0x0085, &bs));
        globals.extend(record(0x000A, &[]));
        assert_eq!(globals.len(), sheet_at);
        let cell = |kind: u16, row: u16, col: u16, xf: u16, rest: &[u8]| {
            let mut b = row.to_le_bytes().to_vec();
            b.extend_from_slice(&col.to_le_bytes());
            b.extend_from_slice(&xf.to_le_bytes());
            b.extend_from_slice(rest);
            record(kind, &b)
        };
        let mut wb = globals;
        wb.extend(record(0x0809, &[0, 6, 0x10, 0]));
        wb.extend(cell(0x00FD, 0, 0, 0, &0u32.to_le_bytes()));
        wb.extend(cell(0x00FD, 0, 1, 0, &1u32.to_le_bytes()));
        wb.extend(cell(0x027E, 1, 0, 0, &((42u32 << 2) | 2).to_le_bytes()));
        wb.extend(cell(0x0203, 1, 1, 1, &45_658f64.to_le_bytes()));
        wb.extend(cell(0x0006, 1, 2, 0, &[0, 0, 0, 0, 0, 0, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0]));
        wb.extend(record(0x0207, &[2, 0, 0, b'h', b'i']));
        wb.extend(record(0x000A, &[]));
        let Ok(Preview::Sheets(sheets)) = parse(&build(&[("Workbook", &wb)])) else { panic!() };
        assert_eq!(sheets[0].name, "Up");
        assert_eq!(sheets[0].rows, vec![vec!["one", "two\u{263A}", ""], vec!["42", "2025-01-01", "hi"]]);
    }

    #[test]
    fn encrypted_packages_are_recognised() {
        assert_eq!(parse(&build(&[("EncryptionInfo", b"x"), ("EncryptedPackage", b"y")])), Err(Error::Encrypted));
    }

    #[test]
    fn rk_numbers() {
        assert_eq!(rk((42 << 2) | 2), 42.0);
        assert_eq!(rk((1234 << 2) | 3), 12.34);
        assert_eq!(rk(((-5i32 << 2) as u32) | 2), -5.0);
        assert_eq!(rk((1.5f64.to_bits() >> 32) as u32), 1.5);
    }
}
