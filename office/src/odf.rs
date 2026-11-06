//! OpenDocument: text (odt), spreadsheets (ods) and presentations (odp),
//! as written by LibreOffice and others.

use crate::ooxml::{split, Body};
use crate::xml::{Tok, Xml};
use crate::zip::Zip;
use crate::{add, Error, Fmt, Grid, Preview, Slide, Span, MAX_COLS, MAX_ROWS, MAX_SHEETS, MAX_SLIDES};
use std::collections::HashMap;

/// The document, if `zip` is an OpenDocument file.
pub fn parse(zip: &Zip) -> Option<Result<Preview, Error>> {
    let mime =
        zip.read("mimetype", 200).map(|(b, _)| String::from_utf8_lossy(&b).trim().to_string()).unwrap_or_default();
    if !zip.has("content.xml") || !(mime.is_empty() || mime.starts_with("application/vnd.oasis.opendocument.")) {
        return None;
    }
    // Password-protected: the manifest describes how the parts are encrypted.
    if zip.text("META-INF/manifest.xml").is_some_and(|(m, _)| m.contains("encryption-data")) {
        return Some(Err(Error::Encrypted));
    }
    let (xml, cut) = zip.text("content.xml")?;
    let is = |kind: &str| {
        mime.contains(&format!(".{kind}")) || (mime.is_empty() && xml.contains(&format!("<office:{kind}")))
    };
    Some(Ok(if is("spreadsheet") {
        sheets(&xml)
    } else if is("presentation") {
        slides(&xml)
    } else {
        text(&xml, cut)
    }))
}

/// Text formatting of the automatic styles (`T1`, `P2`, ...), which come
/// before the body.
fn style_fmt(t: &crate::xml::Tag) -> Fmt {
    Fmt {
        bold: t.attr("font-weight").is_some_and(|w| w == "bold" || w.parse::<u32>().is_ok_and(|n| n >= 600)),
        italic: t.attr("font-style").is_some_and(|s| s == "italic" || s == "oblique"),
        underline: t.attr("text-underline-style").is_some_and(|u| u != "none"),
    }
}

/// Spaces, tabs and line breaks, which are elements in OpenDocument.
fn whitespace(name: &str, t: &crate::xml::Tag) -> Option<String> {
    match name {
        "s" => Some(" ".repeat(t.num::<usize>("c").unwrap_or(1).clamp(1, 100))),
        "tab" => Some("\t".into()),
        "line-break" => Some("\n".into()),
        _ => None,
    }
}

struct Para {
    spans: Vec<Span>,
    heading: Option<u8>,
    list: Option<u8>,
    fmt: Fmt,
}

fn text(xml: &str, cut: bool) -> Preview {
    let mut styles: HashMap<String, Fmt> = HashMap::new();
    let mut style: Option<(String, Fmt)> = None;
    let mut body = Body::default();
    // Open paragraphs (frames put paragraphs in paragraphs) and spans.
    let (mut paras, mut spans) = (Vec::<Para>::new(), Vec::<Fmt>::new());
    let (mut lists, mut in_body) = (0u8, false);
    let mut x = Xml::new(xml);
    while let Some(tok) = x.next() {
        let Some((name, tag, ends)) = split(&tok) else {
            if let (Tok::Text(s), Some(p)) = (&tok, paras.last_mut()) {
                add(&mut p.spans, s, spans.last().copied().unwrap_or(p.fmt));
            }
            continue;
        };
        if let Some(t) = tag {
            let lookup = |styles: &HashMap<String, Fmt>| t.attr("style-name").and_then(|s| styles.get(&s).copied());
            if let (Some(ws), Some(p)) = (whitespace(name, t), paras.last_mut()) {
                add(&mut p.spans, &ws, spans.last().copied().unwrap_or(p.fmt));
            }
            match name {
                "style" if !in_body => style = t.attr("name").map(|n| (n, Fmt::default())),
                "text-properties" if !in_body => {
                    if let Some((_, f)) = style.as_mut() {
                        *f = style_fmt(t);
                    }
                }
                "body" => in_body = true,
                // Footnotes, comments and change tracking are not the text itself.
                "note" | "annotation" | "tracked-changes" if !ends => {
                    x.skip();
                    continue;
                }
                "p" | "h" if in_body => paras.push(Para {
                    spans: Vec::new(),
                    heading: (name == "h").then(|| t.num::<u8>("outline-level").unwrap_or(1)),
                    list: (lists > 0).then(|| lists - 1),
                    fmt: lookup(&styles).unwrap_or_default(),
                }),
                "span" => {
                    let outer = spans.last().copied().or(paras.last().map(|p| p.fmt)).unwrap_or_default();
                    let f = lookup(&styles).unwrap_or_default();
                    spans.push(Fmt {
                        bold: outer.bold || f.bold,
                        italic: outer.italic || f.italic,
                        underline: outer.underline || f.underline,
                    });
                }
                "list" => lists = lists.saturating_add(1),
                "table" if in_body => body.table_start(),
                "table-row" => body.row_start(),
                "table-cell" | "covered-table-cell" => body.cell_start(),
                _ => {}
            }
        }
        if ends {
            match name {
                "style" => {
                    if let Some((n, f)) = style.take() {
                        styles.insert(n, f);
                    }
                }
                "p" | "h" => {
                    if let Some(p) = paras.pop() {
                        if !body.paragraph(p.spans, p.heading, p.list) {
                            break;
                        }
                    }
                }
                "span" => {
                    spans.pop();
                }
                "list" => lists = lists.saturating_sub(1),
                "table-cell" | "covered-table-cell" => body.cell_end(),
                "table-row" => body.row_end(),
                "table" if in_body && !body.table_end() => break,
                _ => {}
            }
        }
    }
    body.finish(cut)
}

fn sheets(xml: &str) -> Preview {
    let mut out = Vec::new();
    let mut grid: Option<(String, Grid)> = None;
    let (mut row, mut col, mut row_repeat) = (0usize, 0usize, 1usize);
    // The current row's cells, and the cell being read (repeat, text).
    let mut row_cells: Vec<(usize, String)> = Vec::new();
    let mut cell: Option<(usize, String)> = None;
    let mut in_p = false;
    let mut x = Xml::new(xml);
    while let Some(tok) = x.next() {
        let Some((name, tag, ends)) = split(&tok) else {
            if let (Tok::Text(s), Some((_, text)), true) = (&tok, cell.as_mut(), in_p) {
                text.push_str(s);
            }
            continue;
        };
        if let Some(t) = tag {
            match name {
                "table" => {
                    if out.len() >= MAX_SHEETS {
                        break;
                    }
                    grid = Some((t.attr("name").unwrap_or_default(), Grid::default()));
                    row = 0;
                }
                "table-row" => {
                    row_repeat = t.num::<usize>("number-rows-repeated").unwrap_or(1).max(1);
                    (row_cells, col) = (Vec::new(), 0);
                }
                "table-cell" | "covered-table-cell" => {
                    cell = Some((t.num::<usize>("number-columns-repeated").unwrap_or(1).max(1), String::new()));
                }
                "p" => {
                    if let Some((_, text)) = cell.as_mut() {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        in_p = true;
                    }
                }
                "annotation" | "shapes" if !ends => {
                    x.skip();
                    continue;
                }
                _ => {
                    if let (true, Some((_, text)), Some(ws)) = (in_p, cell.as_mut(), whitespace(name, t)) {
                        text.push_str(&ws);
                    }
                }
            }
        }
        if ends {
            match name {
                "p" => in_p = false,
                "table-cell" | "covered-table-cell" => {
                    if let Some((repeat, text)) = cell.take() {
                        // Repeats of empty cells (often thousands) take no work.
                        if !text.is_empty() {
                            for k in 0..repeat.min(MAX_COLS.saturating_sub(col).max(1)) {
                                row_cells.push((col + k, text.clone()));
                            }
                        }
                        col = col.saturating_add(repeat);
                    }
                }
                "table-row" => {
                    if let Some((_, g)) = grid.as_mut() {
                        if !row_cells.is_empty() {
                            for r in 0..row_repeat.min(MAX_ROWS.saturating_sub(row).max(1)) {
                                for (c, text) in &row_cells {
                                    g.set(row + r, *c, text.clone());
                                }
                            }
                        }
                    }
                    row = row.saturating_add(row_repeat);
                }
                "table" => {
                    if let Some((name, g)) = grid.take() {
                        out.push(g.into_sheet(name));
                    }
                }
                _ => {}
            }
        }
    }
    Preview::Sheets(out)
}

fn slides(xml: &str) -> Preview {
    let (mut slides, mut cut) = (Vec::new(), false);
    let mut slide: Option<Slide> = None;
    let (mut title_shape, mut lines, mut line, mut in_p) = (false, Vec::<String>::new(), String::new(), 0usize);
    let mut x = Xml::new(xml);
    while let Some(tok) = x.next() {
        let Some((name, tag, ends)) = split(&tok) else {
            if let (Tok::Text(s), true) = (&tok, in_p > 0) {
                line.push_str(s);
            }
            continue;
        };
        if let Some(t) = tag {
            match name {
                "page" => {
                    if slides.len() >= MAX_SLIDES {
                        cut = true;
                        break;
                    }
                    slide = Some(Slide::default());
                }
                // Speaker notes are not on the slide.
                "notes" if !ends => {
                    x.skip();
                    continue;
                }
                "frame" | "custom-shape" => title_shape = t.attr("class").as_deref() == Some("title"),
                "p" | "h" => in_p += 1,
                _ => {
                    if let (true, Some(ws)) = (in_p > 0, whitespace(name, t)) {
                        line.push_str(&ws);
                    }
                }
            }
        }
        if ends {
            match name {
                "p" | "h" => {
                    in_p = in_p.saturating_sub(1);
                    let l = std::mem::take(&mut line);
                    if !l.trim().is_empty() {
                        lines.push(l.trim().to_string());
                    }
                }
                "frame" | "custom-shape" => {
                    if let Some(s) = slide.as_mut() {
                        if title_shape && s.title.is_empty() {
                            s.title = lines.join(" ");
                            lines.clear();
                        } else {
                            s.lines.append(&mut lines);
                        }
                    }
                }
                "page" => {
                    if let Some(mut s) = slide.take() {
                        s.lines.append(&mut lines);
                        slides.push(s);
                    }
                }
                _ => {}
            }
        }
    }
    Preview::Slides { slides, cut }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip::tests::stored;
    use crate::{parse, Block};

    fn odf(mime: &str, content: &str) -> Vec<u8> {
        stored(&[("mimetype", mime.as_bytes()), ("content.xml", content.as_bytes())])
    }

    #[test]
    fn text_documents() {
        let content = r#"<office:document-content><office:automatic-styles>
            <style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>
            </office:automatic-styles><office:body><office:text>
            <text:h text:outline-level="2">Heading</text:h>
            <text:p>A<text:s text:c="2"/><text:span text:style-name="T1">bold</text:span><text:note><text:note-body><text:p>footnote</text:p></text:note-body></text:note></text:p>
            <text:list><text:list-item><text:p>item</text:p></text:list-item></text:list>
            <table:table><table:table-row><table:table-cell><text:p>x</text:p></table:table-cell></table:table-row></table:table>
            </office:text></office:body></office:document-content>"#;
        let Ok(Preview::Document { blocks, .. }) = parse(&odf("application/vnd.oasis.opendocument.text", content))
        else {
            panic!()
        };
        let s = |t: &str, bold: bool| Span { text: t.into(), bold, ..Default::default() };
        assert_eq!(
            blocks,
            vec![
                Block::Heading(2, vec![s("Heading", false)]),
                Block::Paragraph(vec![s("A  ", false), s("bold", true)]),
                Block::ListItem(0, vec![s("item", false)]),
                Block::Table(vec![vec!["x".into()]]),
            ]
        );
    }

    #[test]
    fn spreadsheets_with_repeats() {
        let content = r#"<office:document-content><office:body><office:spreadsheet>
            <table:table table:name="Data">
            <table:table-row><table:table-cell office:value-type="string"><text:p>a</text:p></table:table-cell><table:table-cell table:number-columns-repeated="2"><text:p>r</text:p></table:table-cell><table:table-cell table:number-columns-repeated="16380"/></table:table-row>
            <table:table-row table:number-rows-repeated="1048570"><table:table-cell table:number-columns-repeated="16384"/></table:table-row>
            </table:table></office:spreadsheet></office:body></office:document-content>"#;
        let Ok(Preview::Sheets(sheets)) = parse(&odf("application/vnd.oasis.opendocument.spreadsheet", content)) else {
            panic!()
        };
        assert_eq!(sheets.len(), 1);
        assert_eq!((sheets[0].name.as_str(), sheets[0].cut), ("Data", false));
        assert_eq!(sheets[0].rows, vec![vec!["a".to_string(), "r".into(), "r".into()]]);
    }

    #[test]
    fn presentations() {
        let content = r#"<office:document-content><office:body><office:presentation>
            <draw:page><draw:frame presentation:class="title"><draw:text-box><text:p>Hello</text:p></draw:text-box></draw:frame>
            <draw:frame presentation:class="outline"><draw:text-box><text:list><text:list-item><text:p>one</text:p></text:list-item></text:list></draw:text-box></draw:frame>
            <presentation:notes><draw:frame><draw:text-box><text:p>secret notes</text:p></draw:text-box></draw:frame></presentation:notes></draw:page>
            </office:presentation></office:body></office:document-content>"#;
        let Ok(Preview::Slides { slides, .. }) =
            parse(&odf("application/vnd.oasis.opendocument.presentation", content))
        else {
            panic!()
        };
        assert_eq!(slides, vec![Slide { title: "Hello".into(), lines: vec!["one".into()] }]);
    }

    #[test]
    fn encrypted_files_are_recognised() {
        let z = stored(&[
            ("mimetype", b"application/vnd.oasis.opendocument.text"),
            ("content.xml", b"\x8f\x01garbage"),
            ("META-INF/manifest.xml", b"<manifest:manifest><manifest:file-entry><manifest:encryption-data/></manifest:file-entry></manifest:manifest>"),
        ]);
        assert_eq!(parse(&z), Err(Error::Encrypted));
    }
}
