//! Office Open XML: Word (docx), Excel (xlsx) and PowerPoint (pptx).

use crate::numfmt::{self, Style};
use crate::xml::{Tok, Xml};
use crate::zip::{join, Zip};
use crate::{add, plain, Block, Doc, Error, Fmt, Grid, Preview, Sheet, Slide, Span, MAX_SHEETS, MAX_SLIDES};
use std::collections::HashMap;

/// The document, if `zip` is an Office Open XML package.
pub fn parse(zip: &Zip) -> Option<Result<Preview, Error>> {
    let main =
        rels(zip, "").into_iter().find(|r| r.kind.ends_with("/officeDocument")).map(|r| r.target).or_else(|| {
            ["word/document.xml", "xl/workbook.xml", "ppt/presentation.xml"]
                .into_iter()
                .find(|p| zip.has(p))
                .map(String::from)
        })?;
    Some(match main.split('/').next().unwrap_or("") {
        "word" => docx(zip, &main),
        "xl" => xlsx(zip, &main),
        "ppt" => pptx(zip, &main),
        _ => Err(Error::Unsupported),
    })
}

struct Rel {
    id: String,
    kind: String,
    /// The part it points to.
    target: String,
}

/// The relationships of `part` ("" for the package itself).
fn rels(zip: &Zip, part: &str) -> Vec<Rel> {
    let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
    let Some((text, _)) = zip.text(&join(dir, &format!("_rels/{file}.rels"))) else { return Vec::new() };
    let mut out = Vec::new();
    let mut x = Xml::new(&text);
    while let Some(t) = x.next() {
        if let Tok::Open(t) | Tok::Leaf(t) = t {
            if t.name() == "Relationship" && t.attr("TargetMode").as_deref() != Some("External") {
                if let (Some(id), Some(kind), Some(target)) = (t.attr("Id"), t.attr("Type"), t.attr("Target")) {
                    out.push(Rel { id, kind, target: join(dir, &target) });
                }
            }
        }
    }
    out
}

fn related(rels: &[Rel], kind: &str) -> Option<String> {
    rels.iter().find(|r| r.kind.ends_with(kind)).map(|r| r.target.clone())
}

/// A token as "element starts" and/or "element ends" (an empty element
/// does both), with the element's local name.
pub(crate) fn split<'t, 'a>(tok: &'t Tok<'a>) -> Option<(&'t str, Option<&'t crate::xml::Tag<'a>>, bool)> {
    match tok {
        Tok::Open(t) => Some((t.name(), Some(t), false)),
        Tok::Leaf(t) => Some((t.name(), Some(t), true)),
        Tok::Close(c) => Some((c.name(), None, true)),
        Tok::Text(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Word
// ---------------------------------------------------------------------------

/// Paragraph style ids that are headings, with their level.
fn heading_styles(xml: &str) -> HashMap<String, u8> {
    let mut out = HashMap::new();
    let (mut id, mut level) = (None::<String>, None::<u8>);
    let mut x = Xml::new(xml);
    while let Some(tok) = x.next() {
        let Some((name, tag, ends)) = split(&tok) else { continue };
        if let Some(t) = tag {
            match name {
                "style" => {
                    id = (t.attr("type").as_deref() == Some("paragraph")).then(|| t.attr("styleId")).flatten();
                    level = None;
                }
                // Built-in styles keep their English names, whatever the language.
                "name" if id.is_some() => {
                    let n = t.attr("val").unwrap_or_default().to_ascii_lowercase();
                    if n == "title" {
                        level = Some(1);
                    } else if let Some(l) = n.strip_prefix("heading ").and_then(|l| l.trim().parse::<u8>().ok()) {
                        level = Some(l);
                    }
                }
                "outlineLvl" if id.is_some() => level = level.or(t.num::<u8>("val").filter(|l| *l < 9).map(|l| l + 1)),
                _ => {}
            }
        }
        if ends && name == "style" {
            if let (Some(id), Some(level)) = (id.take(), level) {
                out.insert(id, level.clamp(1, 6));
            }
        }
    }
    out
}

#[derive(Default)]
struct Para {
    spans: Vec<Span>,
    style: Option<String>,
    outline: Option<u8>,
    list: Option<u8>,
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
}

/// Paragraphs, lists and tables, shared by the Word and OpenDocument readers.
#[derive(Default)]
pub(crate) struct Body {
    pub doc: Doc,
    /// Open tables, innermost last; paragraphs in them go into a cell.
    tables: Vec<Table>,
}

impl Body {
    /// Returns false once the document is long enough: stop reading.
    pub fn paragraph(&mut self, spans: Vec<Span>, heading: Option<u8>, list: Option<u8>) -> bool {
        if let Some(t) = self.tables.last_mut() {
            if !t.cell.is_empty() {
                t.cell.push('\n');
            }
            t.cell.push_str(&plain(&spans));
            return true;
        }
        self.doc.push(match (heading, list) {
            (Some(level), _) => Block::Heading(level.clamp(1, 6), spans),
            (None, Some(level)) => Block::ListItem(level.min(8), spans),
            _ => Block::Paragraph(spans),
        })
    }

    pub fn table_start(&mut self) {
        self.tables.push(Table::default());
    }

    pub fn row_start(&mut self) {
        if let Some(t) = self.tables.last_mut() {
            t.row.clear();
        }
    }

    pub fn cell_start(&mut self) {
        if let Some(t) = self.tables.last_mut() {
            t.cell.clear();
        }
    }

    pub fn cell_end(&mut self) {
        if let Some(t) = self.tables.last_mut() {
            let cell = std::mem::take(&mut t.cell);
            t.row.push(cell);
        }
    }

    pub fn row_end(&mut self) {
        if let Some(t) = self.tables.last_mut() {
            let row = std::mem::take(&mut t.row);
            t.rows.push(row);
        }
    }

    /// Returns false once the document is long enough.
    pub fn table_end(&mut self) -> bool {
        let Some(t) = self.tables.pop() else { return true };
        match self.tables.last_mut() {
            // A table in a table: its text goes into the outer cell.
            Some(outer) => {
                let text: Vec<String> = t.rows.iter().map(|r| r.join("\t")).collect();
                outer.cell.push_str(&text.join("\n"));
                true
            }
            None => self.doc.push(Block::Table(t.rows)),
        }
    }

    pub fn finish(self, cut: bool) -> Preview {
        match self.doc.finish() {
            Preview::Document { blocks, cut: c } => Preview::Document { blocks, cut: c || cut },
            other => other,
        }
    }
}

fn docx(zip: &Zip, part: &str) -> Result<Preview, Error> {
    let (xml, cut) = zip.text(part).ok_or(Error::Unsupported)?;
    let headings = related(&rels(zip, part), "/styles")
        .and_then(|p| zip.text(&p))
        .map(|(s, _)| heading_styles(&s))
        .unwrap_or_default();
    let mut body = Body::default();
    // Open paragraphs: text boxes put paragraphs inside paragraphs.
    let mut paras: Vec<Para> = Vec::new();
    let (mut fmt, mut in_run, mut in_text, mut in_ppr, mut in_rpr) = (Fmt::default(), false, false, false, false);
    let mut x = Xml::new(&xml);
    while let Some(tok) = x.next() {
        let Some((name, tag, ends)) = split(&tok) else {
            if let (Tok::Text(s), true, Some(p)) = (&tok, in_text, paras.last_mut()) {
                add(&mut p.spans, s, fmt);
            }
            continue;
        };
        let text = |paras: &mut [Para], s: &str, fmt: Fmt| {
            if let Some(p) = paras.last_mut() {
                add(&mut p.spans, s, fmt);
            }
        };
        if let Some(t) = tag {
            match name {
                // The same content again, for programs that can't read the first.
                "Fallback" if !ends => {
                    x.skip();
                    continue;
                }
                "p" => paras.push(Para::default()),
                "pPr" => in_ppr = true,
                "pStyle" if in_ppr => {
                    if let Some(p) = paras.last_mut() {
                        p.style = t.attr("val");
                    }
                }
                "outlineLvl" if in_ppr => {
                    if let Some(p) = paras.last_mut() {
                        p.outline = t.num::<u8>("val").filter(|l| *l < 9);
                    }
                }
                "numPr" if in_ppr => {
                    if let Some(p) = paras.last_mut() {
                        p.list = Some(0);
                    }
                }
                "ilvl" if in_ppr => {
                    if let Some(p) = paras.last_mut().filter(|p| p.list.is_some()) {
                        p.list = Some(t.num("val").unwrap_or(0));
                    }
                }
                // Numbering id 0 means "no numbering".
                "numId" if in_ppr && t.attr("val").as_deref() == Some("0") => {
                    if let Some(p) = paras.last_mut() {
                        p.list = None;
                    }
                }
                "r" => (in_run, fmt) = (true, Fmt::default()),
                "rPr" if in_run => in_rpr = true,
                "b" if in_rpr => fmt.bold = t.on("val"),
                "i" if in_rpr => fmt.italic = t.on("val"),
                "u" if in_rpr => fmt.underline = t.on("val"),
                "t" if in_run => in_text = true,
                "tab" if in_run && !in_ppr => text(&mut paras, "\t", fmt),
                // Line breaks; page and column breaks are not text.
                "br" | "cr" if in_run && !matches!(t.attr("type").as_deref(), Some("page" | "column")) => {
                    text(&mut paras, "\n", fmt)
                }
                "noBreakHyphen" if in_run => text(&mut paras, "-", fmt),
                "tbl" => body.table_start(),
                "tr" => body.row_start(),
                "tc" => body.cell_start(),
                _ => {}
            }
        }
        if ends {
            match name {
                "pPr" => in_ppr = false,
                "rPr" => in_rpr = false,
                "r" => (in_run, in_text, fmt) = (false, false, Fmt::default()),
                "t" => in_text = false,
                "p" => {
                    if let Some(p) = paras.pop() {
                        let heading = p
                            .outline
                            .map(|l| l + 1)
                            .or_else(|| p.style.as_ref().and_then(|s| headings.get(s).copied()));
                        if !body.paragraph(p.spans, heading, p.list) {
                            break;
                        }
                    }
                }
                "tc" => body.cell_end(),
                "tr" => body.row_end(),
                "tbl" if !body.table_end() => break,
                _ => {}
            }
        }
    }
    Ok(body.finish(cut))
}

// ---------------------------------------------------------------------------
// Excel
// ---------------------------------------------------------------------------

/// The shared strings: rich text runs joined, phonetic hints left out.
fn shared_strings(xml: &str) -> Vec<String> {
    let (mut out, mut cur, mut in_t) = (Vec::new(), String::new(), false);
    let mut x = Xml::new(xml);
    while let Some(tok) = x.next() {
        let Some((name, tag, ends)) = split(&tok) else {
            if let (Tok::Text(s), true) = (&tok, in_t) {
                cur.push_str(s);
            }
            continue;
        };
        if tag.is_some() {
            match name {
                "si" => cur.clear(),
                "rPh" if !ends => x.skip(),
                "t" => in_t = true,
                _ => {}
            }
        }
        if ends {
            match name {
                "t" => in_t = false,
                "si" => out.push(std::mem::take(&mut cur)),
                _ => {}
            }
        }
    }
    out
}

/// The number style of each cell format (by its index).
fn cell_styles(xml: &str) -> Vec<Style> {
    let (mut codes, mut out, mut in_xfs) = (HashMap::new(), Vec::new(), false);
    let mut x = Xml::new(xml);
    while let Some(tok) = x.next() {
        let Some((name, tag, ends)) = split(&tok) else { continue };
        if let Some(t) = tag {
            match name {
                "numFmt" => {
                    if let (Some(id), Some(code)) = (t.num::<u32>("numFmtId"), t.attr("formatCode")) {
                        codes.insert(id, code);
                    }
                }
                "cellXfs" => in_xfs = true,
                "xf" if in_xfs => {
                    let id = t.num::<u32>("numFmtId").unwrap_or(0);
                    out.push(numfmt::style(id, codes.get(&id).map(String::as_str)));
                }
                _ => {}
            }
        }
        if ends && name == "cellXfs" {
            in_xfs = false;
        }
    }
    out
}

/// `B3` -> (row 2, column 1).
fn cell_ref(r: &str) -> Option<(usize, usize)> {
    let split = r.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = r.split_at(split);
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let col = letters.bytes().try_fold(0usize, |acc, b| {
        b.is_ascii_alphabetic().then(|| acc * 26 + usize::from(b.to_ascii_uppercase() - b'A') + 1)
    })?;
    Some((digits.parse::<usize>().ok()?.checked_sub(1)?, col - 1))
}

struct Book<'a> {
    strings: &'a [String],
    styles: &'a [Style],
    date1904: bool,
}

fn sheet(xml: &str, cut: bool, name: String, book: &Book) -> Sheet {
    let mut grid = Grid { cut, ..Grid::default() };
    let (mut row, mut next_row, mut next_col) = (0usize, 0usize, 0usize);
    // The cell being read: position, type, number format, value.
    let mut cell: Option<(usize, usize, Option<String>, usize, String)> = None;
    let (mut in_v, mut in_is, mut in_t) = (false, false, false);
    let mut x = Xml::new(xml);
    while let Some(tok) = x.next() {
        let Some((name, tag, ends)) = split(&tok) else {
            if let (Tok::Text(s), Some(c)) = (&tok, cell.as_mut()) {
                if in_v || (in_is && in_t) {
                    c.4.push_str(s);
                }
            }
            continue;
        };
        if let Some(t) = tag {
            match name {
                "row" => {
                    row = t.num::<usize>("r").and_then(|r| r.checked_sub(1)).unwrap_or(next_row);
                    (next_row, next_col) = (row + 1, 0);
                    if row >= crate::MAX_ROWS {
                        grid.cut = true;
                        break;
                    }
                }
                "c" => {
                    let (r, c) = t.attr("r").and_then(|r| cell_ref(&r)).unwrap_or((row, next_col));
                    next_col = c + 1;
                    cell = Some((r, c, t.attr("t"), t.num("s").unwrap_or(0), String::new()));
                }
                "v" => in_v = true,
                "is" => in_is = true,
                "t" => in_t = true,
                "rPh" if !ends => x.skip(),
                _ => {}
            }
        }
        if ends {
            match name {
                "v" => in_v = false,
                "is" => in_is = false,
                "t" => in_t = false,
                "c" => {
                    if let Some((r, c, kind, style, value)) = cell.take() {
                        grid.set(r, c, cell_text(kind.as_deref(), style, value, book));
                    }
                }
                _ => {}
            }
        }
    }
    grid.into_sheet(name)
}

fn cell_text(kind: Option<&str>, style: usize, value: String, book: &Book) -> String {
    match kind {
        Some("s") => value.trim().parse::<usize>().ok().and_then(|i| book.strings.get(i)).cloned().unwrap_or_default(),
        Some("b") => if value.trim() == "1" { "TRUE" } else { "FALSE" }.to_string(),
        // Text, formula text, errors (#DIV/0!) and ISO dates as they are.
        Some("inlineStr" | "str" | "e" | "d") => value,
        _ => match value.trim().parse::<f64>() {
            Ok(v) => numfmt::show(v, book.styles.get(style).copied().unwrap_or_default(), book.date1904),
            Err(_) => value,
        },
    }
}

fn xlsx(zip: &Zip, part: &str) -> Result<Preview, Error> {
    let (wb, _) = zip.text(part).ok_or(Error::Unsupported)?;
    let rels = rels(zip, part);
    let mut date1904 = false;
    // Visible worksheets in order: (name, part).
    let mut sheets: Vec<(String, String)> = Vec::new();
    let mut x = Xml::new(&wb);
    while let Some(tok) = x.next() {
        let (Tok::Open(t) | Tok::Leaf(t)) = tok else { continue };
        match t.name() {
            "workbookPr" => date1904 = t.attr("date1904").is_some_and(|v| v == "1" || v == "true"),
            "sheet" if !matches!(t.attr("state").as_deref(), Some("hidden" | "veryHidden")) => {
                let rel = t.attr_ns("id").and_then(|id| rels.iter().find(|r| r.id == id));
                if let (Some(name), Some(rel)) = (t.attr("name"), rel) {
                    if rel.kind.ends_with("/worksheet") {
                        sheets.push((name, rel.target.clone()));
                    }
                }
            }
            _ => {}
        }
    }
    let strings = related(&rels, "/sharedStrings")
        .and_then(|p| zip.text(&p))
        .map(|(s, _)| shared_strings(&s))
        .unwrap_or_default();
    let styles = related(&rels, "/styles").and_then(|p| zip.text(&p)).map(|(s, _)| cell_styles(&s)).unwrap_or_default();
    let book = Book { strings: &strings, styles: &styles, date1904 };
    let out: Vec<Sheet> = sheets
        .into_iter()
        .take(MAX_SHEETS)
        .filter_map(|(name, part)| zip.text(&part).map(|(xml, cut)| sheet(&xml, cut, name, &book)))
        .collect();
    if out.is_empty() {
        return Err(Error::Unsupported);
    }
    Ok(Preview::Sheets(out))
}

// ---------------------------------------------------------------------------
// PowerPoint
// ---------------------------------------------------------------------------

fn pptx(zip: &Zip, part: &str) -> Result<Preview, Error> {
    let rels = rels(zip, part);
    let mut order: Vec<String> = Vec::new();
    if let Some((xml, _)) = zip.text(part) {
        let mut x = Xml::new(&xml);
        while let Some(tok) = x.next() {
            if let Tok::Open(t) | Tok::Leaf(t) = tok {
                if t.name() == "sldId" {
                    if let Some(rel) = t.attr_ns("id").and_then(|id| rels.iter().find(|r| r.id == id)) {
                        order.push(rel.target.clone());
                    }
                }
            }
        }
    }
    if order.is_empty() {
        // No list: the slide parts in number order.
        let mut found: Vec<(u32, String)> = zip
            .names()
            .filter_map(|n| {
                let num = n.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?.parse().ok()?;
                Some((num, n.to_string()))
            })
            .collect();
        found.sort();
        order = found.into_iter().map(|(_, n)| n).collect();
    }
    let cut = order.len() > MAX_SLIDES;
    let slides = order.iter().take(MAX_SLIDES).filter_map(|p| zip.text(p)).map(|(xml, _)| slide(&xml)).collect();
    Ok(Preview::Slides { slides, cut })
}

/// One slide's text: the title placeholder, then everything else.
fn slide(xml: &str) -> Slide {
    let mut s = Slide::default();
    let (mut title_shape, mut lines, mut line, mut in_t) = (false, Vec::<String>::new(), String::new(), false);
    let mut x = Xml::new(xml);
    while let Some(tok) = x.next() {
        let Some((name, tag, ends)) = split(&tok) else {
            if let (Tok::Text(t), true) = (&tok, in_t) {
                line.push_str(t);
            }
            continue;
        };
        if let Some(t) = tag {
            match name {
                "Fallback" if !ends => x.skip(),
                "sp" | "graphicFrame" => title_shape = false,
                "ph" => title_shape = matches!(t.attr("type").as_deref(), Some("title" | "ctrTitle")),
                "t" => in_t = true,
                "br" => line.push('\n'),
                _ => {}
            }
        }
        if ends {
            match name {
                "t" => in_t = false,
                "p" => {
                    let l = std::mem::take(&mut line);
                    if !l.trim().is_empty() {
                        lines.push(l.trim().to_string());
                    }
                }
                "sp" | "graphicFrame" => {
                    if title_shape && s.title.is_empty() {
                        s.title = lines.join(" ");
                        lines.clear();
                    } else {
                        s.lines.append(&mut lines);
                    }
                }
                _ => {}
            }
        }
    }
    s.lines.append(&mut lines);
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip::tests::stored;
    use crate::{parse, Span};

    const CT: &str = r#"<?xml version="1.0"?><Types/>"#;

    fn rels(target: &str, kind: &str) -> String {
        format!(
            r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}" Target="{target}"/></Relationships>"#
        )
    }

    fn span(t: &str, bold: bool) -> Span {
        Span { text: t.into(), bold, ..Default::default() }
    }

    #[test]
    fn word_documents() {
        let doc = r#"<w:document xmlns:w="w"><w:body>
            <w:p><w:pPr><w:pStyle w:val="Kop1"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p>
            <w:p><w:r><w:t xml:space="preserve">Plain </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r><w:r><w:rPr><w:b w:val="0"/></w:rPr><w:t> again</w:t><w:tab/><w:t>x</w:t></w:r></w:p>
            <w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="3"/></w:numPr></w:pPr><w:r><w:t>item</w:t></w:r></w:p>
            <w:p><w:r><w:delText>gone</w:delText><w:instrText>PAGE</w:instrText></w:r><w:r><w:t>kept</w:t></w:r></w:p>
            <w:tbl><w:tr><w:tc><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p><w:p><w:r><w:t>c</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
            <w:p><mc:AlternateContent><mc:Choice><w:r><w:t>once</w:t></w:r></mc:Choice><mc:Fallback><w:r><w:t>twice</w:t></w:r></mc:Fallback></mc:AlternateContent></w:p>
            </w:body></w:document>"#;
        let styles = r#"<w:styles><w:style w:type="paragraph" w:styleId="Kop1"><w:name w:val="heading 1"/></w:style></w:styles>"#;
        let z = stored(&[
            ("[Content_Types].xml", CT.as_bytes()),
            ("_rels/.rels", rels("word/document.xml", "officeDocument").as_bytes()),
            ("word/_rels/document.xml.rels", rels("styles.xml", "styles").as_bytes()),
            ("word/document.xml", doc.as_bytes()),
            ("word/styles.xml", styles.as_bytes()),
        ]);
        let Ok(Preview::Document { blocks, cut: false }) = parse(&z) else { panic!("{:?}", parse(&z)) };
        assert_eq!(
            blocks,
            vec![
                Block::Heading(1, vec![span("Title", false)]),
                Block::Paragraph(vec![span("Plain ", false), span("bold", true), span(" again\tx", false)]),
                Block::ListItem(1, vec![span("item", false)]),
                Block::Paragraph(vec![span("kept", false)]),
                Block::Table(vec![vec!["a".into(), "b\nc".into()]]),
                Block::Paragraph(vec![span("once", false)]),
            ]
        );
    }

    #[test]
    fn excel_workbooks() {
        let wb = r#"<workbook><workbookPr/><sheets><sheet name="One" sheetId="1" r:id="rId1"/><sheet name="Gone" sheetId="2" state="hidden" r:id="rId2"/></sheets></workbook>"#;
        let wb_rels = r#"<Relationships>
            <Relationship Id="rId1" Type="x/worksheet" Target="worksheets/sheet1.xml"/>
            <Relationship Id="rId2" Type="x/worksheet" Target="worksheets/sheet2.xml"/>
            <Relationship Id="rId3" Type="x/sharedStrings" Target="sharedStrings.xml"/>
            <Relationship Id="rId4" Type="x/styles" Target="/xl/styles.xml"/></Relationships>"#;
        let sst =
            r#"<sst><si><t>Name</t></si><si><r><t>Ri</t></r><r><t>ch</t></r><rPh><t>ignored</t></rPh></si></sst>"#;
        let styles = r#"<styleSheet><numFmts><numFmt numFmtId="164" formatCode="0.0%"/></numFmts><cellXfs><xf numFmtId="0"/><xf numFmtId="14"/><xf numFmtId="164"/></cellXfs></styleSheet>"#;
        let sheet1 = r#"<worksheet><sheetData>
            <row r="1"><c r="A1" t="s"><v>0</v></c><c r="C1" t="s"><v>1</v></c></row>
            <row r="3"><c r="A3"><v>42</v></c><c r="B3" s="1"><v>45658</v></c><c s="2"><v>0.256</v></c><c r="D3" t="b"><v>1</v></c><c r="E3" t="inlineStr"><is><t>inline</t></is></c></row>
            </sheetData></worksheet>"#;
        let z = stored(&[
            ("_rels/.rels", rels("xl/workbook.xml", "officeDocument").as_bytes()),
            ("xl/workbook.xml", wb.as_bytes()),
            ("xl/_rels/workbook.xml.rels", wb_rels.as_bytes()),
            ("xl/sharedStrings.xml", sst.as_bytes()),
            ("xl/styles.xml", styles.as_bytes()),
            ("xl/worksheets/sheet1.xml", sheet1.as_bytes()),
            ("xl/worksheets/sheet2.xml", b"<worksheet/>"),
        ]);
        let Ok(Preview::Sheets(sheets)) = parse(&z) else { panic!() };
        assert_eq!(sheets.len(), 1, "hidden sheets are left out");
        assert_eq!(sheets[0].name, "One");
        let row = |r: &[&str]| r.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            sheets[0].rows,
            vec![
                row(&["Name", "", "Rich", "", ""]),
                row(&["", "", "", "", ""]),
                row(&["42", "2025-01-01", "25.6%", "TRUE", "inline"])
            ]
        );
        assert_eq!(cell_ref("AA10"), Some((9, 26)));
        assert_eq!(cell_ref("A0"), None);
    }

    #[test]
    fn powerpoint_presentations() {
        let pres = r#"<p:presentation><p:sldIdLst><p:sldId id="257" r:id="rId3"/><p:sldId id="256" r:id="rId2"/></p:sldIdLst></p:presentation>"#;
        let pres_rels = r#"<Relationships><Relationship Id="rId2" Type="x/slide" Target="slides/slide1.xml"/><Relationship Id="rId3" Type="x/slide" Target="slides/slide2.xml"/></Relationships>"#;
        let s1 = r#"<p:sld><p:cSld><p:spTree>
            <p:sp><p:nvSpPr><p:nvPr><p:ph type="body"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>point one</a:t></a:r></a:p><a:p><a:r><a:t>point</a:t></a:r><a:br/><a:r><a:t>two</a:t></a:r></a:p></p:txBody></p:sp>
            <p:sp><p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>First</a:t></a:r></a:p></p:txBody></p:sp>
            </p:spTree></p:cSld></p:sld>"#;
        let z = stored(&[
            ("_rels/.rels", rels("ppt/presentation.xml", "officeDocument").as_bytes()),
            ("ppt/presentation.xml", pres.as_bytes()),
            ("ppt/_rels/presentation.xml.rels", pres_rels.as_bytes()),
            ("ppt/slides/slide1.xml", s1.as_bytes()),
            ("ppt/slides/slide2.xml", b"<p:sld/>"),
        ]);
        let Ok(Preview::Slides { slides, cut: false }) = parse(&z) else { panic!() };
        assert_eq!(slides.len(), 2);
        assert_eq!(slides[0], Slide::default(), "the presentation's order counts");
        assert_eq!(slides[1].title, "First");
        assert_eq!(slides[1].lines, vec!["point one", "point\ntwo"]);
    }
}
