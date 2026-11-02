//! A small layer over quick-xml for the document formats: element and
//! attribute names without their namespace prefix, attribute values, and
//! text with character references resolved. Nothing from a DTD is ever
//! expanded, and a damaged or cut-off document just ends early.

use quick_xml::escape::resolve_xml_entity;
use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use std::borrow::Cow;

pub enum Tok<'a> {
    /// A start tag.
    Open(Tag<'a>),
    /// An empty element (`<x/>`).
    Leaf(Tag<'a>),
    Close(Close<'a>),
    Text(Cow<'a, str>),
}

pub struct Tag<'a>(BytesStart<'a>);
pub struct Close<'a>(BytesEnd<'a>);

fn local(qname: &str) -> &str {
    qname.rsplit_once(':').map_or(qname, |(_, l)| l)
}

impl Tag<'_> {
    pub fn name(&self) -> &str {
        local(self.0.name().0)
    }

    /// The attribute with this local name.
    pub fn attr(&self, name: &str) -> Option<String> {
        self.find(|q| local(q) == name)
    }

    /// The attribute with this local name and a namespace prefix (for
    /// `r:id` next to a plain `id`).
    pub fn attr_ns(&self, name: &str) -> Option<String> {
        self.find(|q| q.contains(':') && local(q) == name)
    }

    /// A number attribute (`None` if missing or invalid).
    pub fn num<T: std::str::FromStr>(&self, name: &str) -> Option<T> {
        self.attr(name)?.trim().parse().ok()
    }

    /// An on/off attribute: absent means on (`<w:b/>`), as do most values but
    /// `0`, `false` and `off`.
    pub fn on(&self, name: &str) -> bool {
        !matches!(self.attr(name).as_deref(), Some("0" | "false" | "off" | "none"))
    }

    fn find(&self, pred: impl Fn(&str) -> bool) -> Option<String> {
        let attr = self.0.attributes().with_checks(false).flatten().find(|a| pred(a.key.0))?;
        Some(match attr.normalized_value(XmlVersion::Implicit1_0) {
            Ok(v) => v.into_owned(),
            Err(_) => attr.value.into_owned(),
        })
    }
}

impl Close<'_> {
    pub fn name(&self) -> &str {
        local(self.0.name().0)
    }
}

pub struct Xml<'a>(Reader<&'a [u8]>);

impl<'a> Xml<'a> {
    pub fn new(text: &'a str) -> Self {
        let mut reader = Reader::from_str(text);
        let config = reader.config_mut();
        // Be lenient: documents from many programs, some of them sloppy.
        config.check_end_names = false;
        config.allow_unmatched_ends = true;
        Xml(reader)
    }

    /// The next token, or None at the end (or where the document is
    /// damaged or cut off).
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Tok<'a>> {
        loop {
            return Some(match self.0.read_event().ok()? {
                Event::Start(t) => Tok::Open(Tag(t)),
                Event::Empty(t) => Tok::Leaf(Tag(t)),
                Event::End(t) => Tok::Close(Close(t)),
                Event::Text(t) => Tok::Text(t.xml10_content()),
                Event::CData(t) => Tok::Text(t.xml10_content()),
                Event::GeneralRef(r) => {
                    let text = match r.resolve_char_ref() {
                        Ok(Some(c)) => c.to_string(),
                        _ => resolve_xml_entity(r.as_ref()).unwrap_or_default().to_string(),
                    };
                    Tok::Text(Cow::Owned(text))
                }
                Event::Eof => return None,
                // Declarations, comments, processing instructions, DTDs.
                _ => continue,
            });
        }
    }

    /// Skip the rest of the element whose start tag was just read.
    pub fn skip(&mut self) {
        let mut depth = 1usize;
        while let Some(t) = self.next() {
            match t {
                Tok::Open(_) => depth += 1,
                Tok::Close(_) => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_attributes_and_text() {
        let mut x = Xml::new(r#"<?xml version="1.0"?><w:p a:id="1" id="2" w:val="x &amp; y"><w:t>A &lt;b&gt; &#x263A;</w:t><w:b/></w:p>"#);
        let Some(Tok::Open(p)) = x.next() else { panic!() };
        assert_eq!(p.name(), "p");
        assert_eq!(p.attr_ns("id").as_deref(), Some("1"));
        assert_eq!(p.attr("val").as_deref(), Some("x & y"));
        assert!(matches!(x.next(), Some(Tok::Open(t)) if t.name() == "t"));
        let mut text = String::new();
        while let Some(Tok::Text(t)) = x.next() {
            text.push_str(&t);
        }
        assert_eq!(text, "A <b> \u{263A}");
        assert!(matches!(x.next(), Some(Tok::Leaf(t)) if t.name() == "b" && t.on("val")));
    }

    #[test]
    fn entities_from_a_dtd_are_not_expanded() {
        let mut x = Xml::new(r#"<!DOCTYPE x [<!ENTITY e "boom">]><x>&e;</x>"#);
        let mut text = String::new();
        while let Some(t) = x.next() {
            if let Tok::Text(t) = t {
                text.push_str(&t);
            }
        }
        assert_eq!(text, "");
    }

    #[test]
    fn cut_off_documents_just_end() {
        let mut x = Xml::new("<a><b>text</b><c attr=\"un");
        let mut n = 0;
        while x.next().is_some() {
            n += 1;
        }
        // <a>, <b>, text, </b>; the cut-off tag gives nothing.
        assert_eq!(n, 4);
    }
}
