//! A small tree of an XML part, for reading Word and OpenDocument files.
//! Each element keeps its namespace, so Word's `w:p` and DrawingML's `a:p`
//! stay apart whatever prefixes a file uses.

use std::io::BufRead;

use quick_xml::NsReader;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;

/// The namespaces the readers look at. Others are [`Ns::Other`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ns {
    /// No namespace.
    None,
    /// WordprocessingML, `w:`.
    W,
    /// Relationships in a part, `r:`.
    R,
    /// DrawingML, `a:`.
    A,
    /// Word's drawings, `wp:`.
    Wp,
    /// DrawingML pictures, `pic:`.
    Pic,
    /// Markup compatibility, `mc:`.
    Mc,
    /// VML, `v:`, in older files.
    V,
    /// Office math, `m:`.
    M,
    /// Word's shapes, `wps:`.
    Wps,
    /// A package's relationships part.
    Rel,
    /// OpenDocument's parts.
    Office,
    Text,
    Style,
    Fo,
    Table,
    Draw,
    Svg,
    Xlink,
    /// The `xml:` prefix, as in `xml:space`.
    Xml,
    Other,
}

fn namespace(uri: &str) -> Ns {
    match uri {
        "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
        | "http://purl.oclc.org/ooxml/wordprocessingml/main" => Ns::W,
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
        | "http://purl.oclc.org/ooxml/officeDocument/relationships" => Ns::R,
        "http://schemas.openxmlformats.org/drawingml/2006/main"
        | "http://purl.oclc.org/ooxml/drawingml/main" => Ns::A,
        "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
        | "http://purl.oclc.org/ooxml/drawingml/wordprocessingDrawing" => Ns::Wp,
        "http://schemas.openxmlformats.org/drawingml/2006/picture"
        | "http://purl.oclc.org/ooxml/drawingml/picture" => Ns::Pic,
        "http://schemas.openxmlformats.org/markup-compatibility/2006" => Ns::Mc,
        "urn:schemas-microsoft-com:vml" => Ns::V,
        "http://schemas.openxmlformats.org/officeDocument/2006/math"
        | "http://purl.oclc.org/ooxml/officeDocument/math" => Ns::M,
        "http://schemas.microsoft.com/office/word/2010/wordprocessingShape" => Ns::Wps,
        "http://schemas.openxmlformats.org/package/2006/relationships" => Ns::Rel,
        "urn:oasis:names:tc:opendocument:xmlns:office:1.0" => Ns::Office,
        "urn:oasis:names:tc:opendocument:xmlns:text:1.0" => Ns::Text,
        "urn:oasis:names:tc:opendocument:xmlns:style:1.0" => Ns::Style,
        "urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" => Ns::Fo,
        "urn:oasis:names:tc:opendocument:xmlns:table:1.0" => Ns::Table,
        "urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" => Ns::Draw,
        "urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" => Ns::Svg,
        "http://www.w3.org/1999/xlink" => Ns::Xlink,
        "http://www.w3.org/XML/1998/namespace" => Ns::Xml,
        _ => Ns::Other,
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Element {
    pub ns: Option<Ns>,
    pub name: String,
    attributes: Vec<(Ns, String, String)>,
    pub children: Vec<Node>,
}

#[derive(Clone, Debug)]
pub(crate) enum Node {
    Element(Element),
    Text(String),
}

impl Element {
    /// Whether this is `name` in `ns`.
    pub fn is(&self, ns: Ns, name: &str) -> bool {
        self.ns == Some(ns) && self.name == name
    }

    /// The attribute `name` in `ns`, such as `w:val`.
    pub fn attr(&self, ns: Ns, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(at, local, _)| *at == ns && local == name)
            .map(|(_, _, value)| value.as_str())
    }

    /// Word's `w:val`.
    pub fn val(&self) -> Option<&str> {
        self.attr(Ns::W, "val")
    }

    /// The child elements.
    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|node| match node {
            Node::Element(element) => Some(element),
            Node::Text(_) => None,
        })
    }

    /// The first child element that's `name` in `ns`.
    pub fn child(&self, ns: Ns, name: &str) -> Option<&Element> {
        self.elements().find(|element| element.is(ns, name))
    }

    /// Every element below this one that's `name` in `ns`, depth first.
    pub fn descendants<'a>(&'a self, ns: Ns, name: &'a str) -> Vec<&'a Element> {
        let mut found = Vec::new();
        let mut waiting: Vec<&Element> = self.elements().collect();
        waiting.reverse();
        while let Some(element) = waiting.pop() {
            if element.is(ns, name) {
                found.push(element);
            }
            let start = waiting.len();
            waiting.extend(element.elements());
            waiting[start..].reverse();
        }
        found
    }

    /// The text directly inside this element.
    pub fn text(&self) -> String {
        let mut text = String::new();
        for node in &self.children {
            if let Node::Text(words) = node {
                text.push_str(words);
            }
        }
        text
    }
}

/// The most elements one part may have, far more than any real document,
/// so a file made to exhaust memory stops here.
const MAX_ELEMENTS: usize = 4_000_000;

/// How deep elements may go.
const MAX_DEPTH: usize = 256;

/// The part's root element.
pub(crate) fn parse(input: impl BufRead) -> Result<Element, String> {
    let mut reader = NsReader::from_reader(input);
    let mut buf = Vec::new();
    // The elements still open, the root's container first.
    let mut open: Vec<Element> = vec![Element::default()];
    let mut count = 0usize;
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|err| format!("isn't XML Docs can read: {err}"))?;
        match event {
            Event::Start(start) | Event::Empty(start) if open.len() > MAX_DEPTH => {
                let _ = start;
                return Err("goes too many elements deep".into());
            }
            Event::Start(start) => {
                count += 1;
                open.push(element(&reader, &start)?);
            }
            Event::Empty(start) => {
                count += 1;
                let made = element(&reader, &start)?;
                push(&mut open, Node::Element(made));
            }
            Event::End(_) => {
                if open.len() > 1 {
                    let done = open.pop().expect("an open element");
                    push(&mut open, Node::Element(done));
                }
            }
            Event::Text(text) => push_text(&mut open, &text.xml10_content()),
            Event::CData(text) => push_text(&mut open, &text.into_inner()),
            Event::GeneralRef(reference) => {
                if let Ok(Some(character)) = reference.resolve_char_ref() {
                    push_text(&mut open, character.encode_utf8(&mut [0; 4]));
                } else {
                    let name = reference.into_inner();
                    if let Some(entity) = quick_xml::escape::resolve_predefined_entity(&name) {
                        push_text(&mut open, entity);
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        if count > MAX_ELEMENTS {
            return Err("has too many elements".into());
        }
        buf.clear();
    }
    // An unclosed element still counts, as browsers read broken pages.
    while open.len() > 1 {
        let done = open.pop().expect("an open element");
        push(&mut open, Node::Element(done));
    }
    let root = open.pop().expect("the root's container");
    root.children
        .into_iter()
        .find_map(|node| match node {
            Node::Element(element) => Some(element),
            Node::Text(_) => None,
        })
        .ok_or_else(|| "has no elements".to_owned())
}

fn push(open: &mut [Element], node: Node) {
    open.last_mut()
        .expect("an open element")
        .children
        .push(node);
}

fn push_text(open: &mut [Element], text: &str) {
    let parent = open.last_mut().expect("an open element");
    if let Some(Node::Text(last)) = parent.children.last_mut() {
        last.push_str(text);
    } else {
        parent.children.push(Node::Text(text.to_owned()));
    }
}

fn element<R>(reader: &NsReader<R>, start: &BytesStart) -> Result<Element, String> {
    let resolver = reader.resolver();
    let (ns, local) = resolver.resolve_element(start.name());
    let ns = match ns {
        ResolveResult::Bound(uri) => Some(namespace(uri.into_inner())),
        ResolveResult::Unbound => None,
        ResolveResult::Unknown(_) => Some(Ns::Other),
    };
    let mut attributes = Vec::new();
    for attribute in start.attributes().with_checks(false) {
        let attribute = attribute.map_err(|err| format!("has a broken attribute: {err}"))?;
        let (at, local) = resolver.resolve_attribute(attribute.key);
        let at = match at {
            ResolveResult::Bound(uri) => namespace(uri.into_inner()),
            ResolveResult::Unbound => Ns::None,
            ResolveResult::Unknown(prefix) if prefix == "xml" => Ns::Xml,
            ResolveResult::Unknown(_) => Ns::Other,
        };
        if attribute.key.into_inner().starts_with("xmlns") {
            continue;
        }
        let value = attribute
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map(|value| value.into_owned())
            .unwrap_or_else(|_| attribute.value.clone().into_owned());
        attributes.push((at, local.into_inner().to_owned(), value));
    }
    Ok(Element {
        ns,
        name: local.into_inner().to_owned(),
        attributes,
        children: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_namespaces_whatever_the_prefix() {
        let root = parse(
            &br#"<x:document xmlns:x="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><x:p x:rsid="1"><x:t xml:space="preserve"> A &amp; B&#x2603;</x:t><a:p/></x:p></x:document>"#[..],
        )
        .unwrap();
        assert!(root.is(Ns::W, "document"));
        let p = root.child(Ns::W, "p").unwrap();
        assert_eq!(p.attr(Ns::W, "rsid"), Some("1"));
        let t = p.child(Ns::W, "t").unwrap();
        assert_eq!(t.text(), " A & B☃");
        assert_eq!(t.attr(Ns::Xml, "space"), Some("preserve"));
        assert!(p.child(Ns::A, "p").is_some());
        assert_eq!(root.descendants(Ns::W, "t").len(), 1);
    }

    #[test]
    fn refuses_elements_too_deep() {
        let deep = "<a>".repeat(MAX_DEPTH + 10);
        assert!(parse(deep.as_bytes()).is_err());
    }
}
