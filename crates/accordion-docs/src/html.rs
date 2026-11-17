//! A document's file: an ordinary HTML page that any browser shows, with
//! the co-editing state in a data block browsers neither run nor show
//! (decision 30).
//!
//! The server writes the body from the schema in [`crate::model`], so only
//! its elements and attributes reach the file: never a script, a style
//! attribute, or an event handler. Reading goes the other way, through the
//! same schema, for HTML Docs wrote and HTML any other program wrote.
//!
//! The writer puts each block on a line of its own, and nothing else between
//! a block's tags, so the reader gets back exactly what was written.
//! Spaces typed inside text are kept as typed. Only text with line breaks or
//! tabs in it, which is how other programs lay out their HTML, has its
//! whitespace folded, as a browser shows it.

use std::fmt::Write as _;

use scraper::{Html, Node};

use crate::model::{self, Block, Cell, Inline, Marks};

/// How every document starts. Docs recognizes its files by it, so a web page
/// someone uploads stays a web page.
pub const MARKER: &str = "<!DOCTYPE html>\n<html data-accordion-docs=\"1\"";

/// The data block's type. Browsers skip scripts of a type they don't know.
const DATA_TYPE: &str = "application/x-accordion-docs";

/// The format version in the data block.
pub const DATA_VERSION: u32 = 1;

/// How the page looks on screen and in print, kept small. The editor in the
/// web app has its own styles.
const STYLE: &str = "\
body{margin:0;background:#f8f9fa;color:#1f1f1f;font:16px/1.6 system-ui,sans-serif}\
main{box-sizing:border-box;max-width:816px;margin:24px auto;padding:72px 96px;background:#fff;\
box-shadow:0 1px 3px rgba(0,0,0,.15)}\
p,h1,h2,h3,h4,h5,h6,li,td,th{white-space:pre-wrap}\
p{margin:0 0 .6em}\
pre{background:#f1f3f4;padding:12px;border-radius:4px;overflow-x:auto;white-space:pre-wrap}\
code{font-family:ui-monospace,monospace;font-size:.9em}\
blockquote{margin:0 0 .6em;padding-left:16px;border-left:3px solid #dadce0;color:#444746}\
table{border-collapse:collapse;margin:0 0 .6em}\
td,th{border:1px solid #c4c7c5;padding:4px 8px;vertical-align:top;min-width:40px}\
th{background:#f1f3f4;text-align:left}\
td>p:last-child,th>p:last-child,li>p:last-child{margin:0}\
ul.checklist{list-style:none;padding-left:4px}\
ul.checklist>li>input{float:left;margin:6px 8px 0 0}\
ul.checklist>li[data-checked=true]>p{text-decoration:line-through;color:#5f6368}\
@media (max-width:860px){main{margin:0;padding:24px 16px;box-shadow:none}}\
@media print{body{background:none}main{max-width:none;margin:0;padding:0;box-shadow:none}}\
@media (prefers-color-scheme:dark){body{background:#131314;color:#e3e3e3}\
main{background:#1f1f1f}pre,th{background:#2d2f31}td,th{border-color:#444746}\
blockquote{border-color:#444746;color:#c4c7c5}}";

/// The whole file for `blocks`, with `state`, the co-editing state, in its
/// data block when there is one.
pub fn page(blocks: &[Block], state: Option<&[u8]>) -> String {
    let title = model::first_text(blocks)
        .map(|text| text.chars().take(100).collect::<String>())
        .unwrap_or_else(|| "Untitled document".to_owned());
    let mut out = String::with_capacity(4096);
    out.push_str(MARKER);
    out.push_str(">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<meta name=\"generator\" content=\"Accordion Docs\">\n");
    let _ = writeln!(out, "<title>{}</title>", escape_text(&title));
    let _ = writeln!(out, "<style>{STYLE}</style>");
    out.push_str("</head>\n<body>\n<main>\n");
    write_blocks(&mut out, blocks);
    out.push_str("</main>\n");
    if let Some(state) = state {
        use base64::Engine as _;
        let data = serde_json::json!({
            "version": DATA_VERSION,
            "state": base64::engine::general_purpose::STANDARD.encode(state),
        });
        let _ = writeln!(out, "<script type=\"{DATA_TYPE}\">{data}</script>");
    }
    out.push_str("</body>\n</html>\n");
    out
}

/// Whether `contents` is a Docs document.
pub fn is_document(contents: &[u8]) -> bool {
    contents.starts_with(MARKER.as_bytes())
}

/// The co-editing state in a document's data block, if it has one Docs can
/// read.
pub fn state_in(contents: &[u8]) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let text = std::str::from_utf8(contents).ok()?;
    let open = format!("<script type=\"{DATA_TYPE}\">");
    let start = text.rfind(&open)? + open.len();
    let end = start + text[start..].find("</script>")?;
    let data: serde_json::Value = serde_json::from_str(&text[start..end]).ok()?;
    if data.get("version")?.as_u64()? != u64::from(DATA_VERSION) {
        return None;
    }
    base64::engine::general_purpose::STANDARD
        .decode(data.get("state")?.as_str()?)
        .ok()
}

fn write_blocks(out: &mut String, blocks: &[Block]) {
    for block in blocks {
        write_block(out, block);
    }
}

fn write_block(out: &mut String, block: &Block) {
    match block {
        Block::Paragraph(inlines) => {
            out.push_str("<p>");
            write_inlines(out, inlines);
            out.push_str("</p>\n");
        }
        Block::Heading(level, inlines) => {
            let _ = write!(out, "<h{level}>");
            write_inlines(out, inlines);
            let _ = writeln!(out, "</h{level}>");
        }
        Block::Quote(blocks) => {
            out.push_str("<blockquote>\n");
            write_blocks(out, blocks);
            out.push_str("</blockquote>\n");
        }
        Block::Code(text) => {
            let _ = writeln!(out, "<pre><code>{}</code></pre>", escape_text(text));
        }
        Block::Rule => out.push_str("<hr>\n"),
        Block::Bullets(items) => {
            out.push_str("<ul>\n");
            for blocks in items {
                out.push_str("<li>\n");
                write_blocks(out, blocks);
                out.push_str("</li>\n");
            }
            out.push_str("</ul>\n");
        }
        Block::Numbers(start, items) => {
            if *start == 1 {
                out.push_str("<ol>\n");
            } else {
                let _ = writeln!(out, "<ol start=\"{start}\">");
            }
            for blocks in items {
                out.push_str("<li>\n");
                write_blocks(out, blocks);
                out.push_str("</li>\n");
            }
            out.push_str("</ol>\n");
        }
        Block::Checks(items) => {
            out.push_str("<ul class=\"checklist\">\n");
            for (checked, blocks) in items {
                if *checked {
                    out.push_str(
                        "<li data-checked=\"true\"><input type=\"checkbox\" checked disabled>\n",
                    );
                } else {
                    out.push_str("<li data-checked=\"false\"><input type=\"checkbox\" disabled>\n");
                }
                write_blocks(out, blocks);
                out.push_str("</li>\n");
            }
            out.push_str("</ul>\n");
        }
        Block::Table(rows) => {
            out.push_str("<table>\n<tbody>\n");
            for row in rows {
                out.push_str("<tr>\n");
                for cell in row {
                    let tag = if cell.header { "th" } else { "td" };
                    let _ = write!(out, "<{tag}");
                    if cell.colspan != 1 {
                        let _ = write!(out, " colspan=\"{}\"", cell.colspan);
                    }
                    if cell.rowspan != 1 {
                        let _ = write!(out, " rowspan=\"{}\"", cell.rowspan);
                    }
                    if let Some(widths) = &cell.colwidth {
                        let widths: Vec<String> = widths.iter().map(u32::to_string).collect();
                        let _ = write!(out, " data-colwidth=\"{}\"", widths.join(","));
                    }
                    out.push_str(">\n");
                    write_blocks(out, &cell.content);
                    let _ = writeln!(out, "</{tag}>");
                }
                out.push_str("</tr>\n");
            }
            out.push_str("</tbody>\n</table>\n");
        }
    }
}

/// A mark as a tag, in the order they nest, outermost first.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Tag {
    Link(String),
    Bold,
    Italic,
    Underline,
    Strike,
    Code,
}

impl Tag {
    fn of(marks: &Marks) -> Vec<Tag> {
        let mut tags = Vec::new();
        if let Some(href) = &marks.link {
            tags.push(Tag::Link(href.clone()));
        }
        for (on, tag) in [
            (marks.bold, Tag::Bold),
            (marks.italic, Tag::Italic),
            (marks.underline, Tag::Underline),
            (marks.strike, Tag::Strike),
            (marks.code, Tag::Code),
        ] {
            if on {
                tags.push(tag);
            }
        }
        tags
    }

    fn open(&self, out: &mut String) {
        match self {
            Tag::Link(href) => {
                let _ = write!(out, "<a href=\"{}\">", escape_attribute(href));
            }
            Tag::Bold => out.push_str("<strong>"),
            Tag::Italic => out.push_str("<em>"),
            Tag::Underline => out.push_str("<u>"),
            Tag::Strike => out.push_str("<s>"),
            Tag::Code => out.push_str("<code>"),
        }
    }

    fn close(&self, out: &mut String) {
        out.push_str(match self {
            Tag::Link(_) => "</a>",
            Tag::Bold => "</strong>",
            Tag::Italic => "</em>",
            Tag::Underline => "</u>",
            Tag::Strike => "</s>",
            Tag::Code => "</code>",
        });
    }
}

/// Text with its marks, keeping tags open across runs that share them.
fn write_inlines(out: &mut String, inlines: &[Inline]) {
    let mut open: Vec<Tag> = Vec::new();
    for inline in inlines {
        match inline {
            Inline::Text(text, marks) => {
                let wanted = Tag::of(marks);
                let same = open
                    .iter()
                    .zip(&wanted)
                    .take_while(|(open, wanted)| open == wanted)
                    .count();
                while open.len() > same {
                    open.pop().expect("longer than same").close(out);
                }
                for tag in &wanted[same..] {
                    tag.open(out);
                    open.push(tag.clone());
                }
                out.push_str(&escape_text(text));
            }
            Inline::Break => out.push_str("<br>"),
        }
    }
    while let Some(tag) = open.pop() {
        tag.close(out);
    }
}

fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            c => out.push(c),
        }
    }
    out
}

fn escape_attribute(text: &str) -> String {
    escape_text(text).replace('"', "&quot;")
}

/// The blocks of an HTML page, through Docs' schema. Everything the schema
/// doesn't have is left out: scripts, styles, forms, and so on. Text in
/// elements the schema doesn't know stays, as plain text.
pub fn read(contents: &[u8]) -> Vec<Block> {
    let text = String::from_utf8_lossy(contents);
    let html = Html::parse_document(&text);
    let root = html.root_element();
    // Docs writes its blocks in <main>. Other pages are read whole.
    let main = root
        .descendants()
        .find(|node| matches!(node.value(), Node::Element(el) if el.name() == "main"));
    let mut blocks = Vec::new();
    match main {
        Some(main) => read_blocks(main, &mut blocks),
        None => read_blocks(*root, &mut blocks),
    }
    model::normalize(blocks)
}

type NodeRef<'a> = ego_tree::NodeRef<'a, Node>;

/// Elements whose contents are never part of a document.
const SKIPPED: &[&str] = &[
    "head", "script", "style", "template", "noscript", "iframe", "object", "embed", "svg", "math",
    "title", "meta", "link", "input", "button", "select", "textarea", "img", "video", "audio",
    "canvas", "picture", "source", "track", "map", "area", "dialog",
];

/// Inline text gathered between blocks, for a paragraph of its own.
#[derive(Default)]
struct Runs {
    runs: Vec<Run>,
}

/// Text read from HTML, and whether its whitespace was folded, so that
/// space at a block's start and end can be dropped, as browsers do.
enum Run {
    Text {
        text: String,
        marks: Marks,
        folded: bool,
    },
    Break,
}

impl Runs {
    fn is_blank(&self) -> bool {
        self.runs.iter().all(|run| match run {
            Run::Text { text, folded, .. } => *folded && text.trim().is_empty(),
            Run::Break => false,
        })
    }

    /// The text as inlines, with folded space at either end trimmed.
    fn finish(mut self) -> Vec<Inline> {
        if let Some(Run::Text {
            text, folded: true, ..
        }) = self.runs.first_mut()
        {
            *text = text.trim_start().to_owned();
        }
        if let Some(Run::Text {
            text, folded: true, ..
        }) = self.runs.last_mut()
        {
            *text = text.trim_end().to_owned();
        }
        // A folded space next to a line break is layout, not text.
        let count = self.runs.len();
        for index in 0..count {
            let after_break = index > 0 && matches!(self.runs[index - 1], Run::Break);
            let before_break = matches!(self.runs.get(index + 1), Some(Run::Break));
            if let Run::Text {
                text, folded: true, ..
            } = &mut self.runs[index]
            {
                if after_break {
                    *text = text.trim_start().to_owned();
                }
                if before_break {
                    *text = text.trim_end().to_owned();
                }
            }
        }
        model::normalize_inlines(
            self.runs
                .into_iter()
                .map(|run| match run {
                    Run::Text { text, marks, .. } => Inline::Text(text, marks),
                    Run::Break => Inline::Break,
                })
                .collect(),
        )
    }
}

fn element(node: &NodeRef) -> Option<&'static str> {
    let Node::Element(el) = node.value() else {
        return None;
    };
    // Matching against a fixed list gives a 'static name.
    const KNOWN: &[&str] = &[
        "p",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "blockquote",
        "pre",
        "hr",
        "ul",
        "ol",
        "li",
        "table",
        "thead",
        "tbody",
        "tfoot",
        "tr",
        "td",
        "th",
        "br",
        "strong",
        "b",
        "em",
        "i",
        "u",
        "ins",
        "s",
        "strike",
        "del",
        "code",
        "kbd",
        "samp",
        "tt",
        "a",
        "div",
        "section",
        "article",
        "main",
        "header",
        "footer",
        "nav",
        "aside",
        "figure",
        "figcaption",
        "body",
        "html",
        "center",
        "form",
        "fieldset",
        "details",
        "summary",
        "address",
        "dl",
        "dd",
        "dt",
        "caption",
        "colgroup",
        "col",
    ];
    let name = el.name();
    KNOWN
        .iter()
        .chain(SKIPPED)
        .find(|known| **known == name)
        .copied()
        .or(Some("other"))
}

fn attribute<'a>(node: &NodeRef<'a>, name: &str) -> Option<&'a str> {
    match node.value() {
        Node::Element(el) => el.attr(name),
        _ => None,
    }
}

/// Reads the children of `parent` as blocks.
fn read_blocks(parent: NodeRef, out: &mut Vec<Block>) {
    let mut runs = Runs::default();
    let flush = |runs: &mut Runs, out: &mut Vec<Block>| {
        let taken = std::mem::take(runs);
        if !taken.is_blank() {
            out.push(Block::Paragraph(taken.finish()));
        }
    };
    for child in parent.children() {
        let name = match child.value() {
            Node::Text(text) => {
                add_text(&mut runs, &text[..], &Marks::default());
                continue;
            }
            Node::Element(_) => element(&child).unwrap_or("other"),
            _ => continue,
        };
        if SKIPPED.contains(&name) {
            continue;
        }
        match name {
            "p" => {
                flush(&mut runs, out);
                out.push(Block::Paragraph(inlines_of(child)));
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                flush(&mut runs, out);
                let level = name[1..].parse().unwrap_or(1);
                out.push(Block::Heading(level, inlines_of(child)));
            }
            "blockquote" => {
                flush(&mut runs, out);
                let mut blocks = Vec::new();
                read_blocks(child, &mut blocks);
                out.push(Block::Quote(blocks));
            }
            "pre" => {
                flush(&mut runs, out);
                out.push(Block::Code(text_of(child)));
            }
            "hr" => {
                flush(&mut runs, out);
                out.push(Block::Rule);
            }
            "ul" | "ol" => {
                flush(&mut runs, out);
                out.push(list(child, name == "ol"));
            }
            "table" => {
                flush(&mut runs, out);
                out.push(table(child));
            }
            "br" => runs.runs.push(Run::Break),
            "div" | "section" | "article" | "main" | "header" | "footer" | "nav" | "aside"
            | "figure" | "figcaption" | "body" | "html" | "center" | "form" | "fieldset"
            | "details" | "summary" | "address" | "dl" | "dd" | "dt" | "li" | "thead" | "tbody"
            | "tfoot" | "tr" | "td" | "th" | "caption" => {
                flush(&mut runs, out);
                read_blocks(child, out);
            }
            "colgroup" | "col" => {}
            _ => read_inline(child, &Marks::default(), &mut runs),
        }
    }
    flush(&mut runs, out);
}

/// The inline contents of a paragraph or heading.
fn inlines_of(node: NodeRef) -> Vec<Inline> {
    let mut runs = Runs::default();
    for child in node.children() {
        read_inline(child, &Marks::default(), &mut runs);
    }
    runs.finish()
}

fn add_text(runs: &mut Runs, text: &str, marks: &Marks) {
    let folded = text.contains(['\n', '\r', '\t', '\x0c']);
    let text = if folded { fold(text) } else { text.to_owned() };
    runs.runs.push(Run::Text {
        text,
        marks: marks.clone(),
        folded,
    });
}

/// Folds each stretch of HTML whitespace into one space, as browsers show
/// it.
fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if matches!(c, ' ' | '\n' | '\r' | '\t' | '\x0c') {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out
}

fn read_inline(node: NodeRef, marks: &Marks, runs: &mut Runs) {
    let name = match node.value() {
        Node::Text(text) => {
            add_text(runs, &text[..], marks);
            return;
        }
        Node::Element(_) => element(&node).unwrap_or("other"),
        _ => return,
    };
    if SKIPPED.contains(&name) {
        return;
    }
    let mut marks = marks.clone();
    match name {
        "br" => {
            runs.runs.push(Run::Break);
            return;
        }
        "strong" | "b" => marks.bold = true,
        "em" | "i" => marks.italic = true,
        "u" | "ins" => marks.underline = true,
        "s" | "strike" | "del" => marks.strike = true,
        "code" | "kbd" | "samp" | "tt" => marks.code = true,
        "a" => {
            if let Some(href) = attribute(&node, "href").and_then(model::safe_link) {
                marks.link = Some(href);
            }
        }
        _ => {}
    }
    for child in node.children() {
        read_inline(child, &marks, runs);
    }
}

/// All the text inside, as it is, for code.
fn text_of(node: NodeRef) -> String {
    let mut out = String::new();
    for descendant in node.descendants() {
        match descendant.value() {
            Node::Text(text) => out.push_str(&text[..]),
            Node::Element(el) if el.name() == "br" => out.push('\n'),
            _ => {}
        }
    }
    out
}

fn list(node: NodeRef, numbered: bool) -> Block {
    let checklist = attribute(&node, "class")
        .is_some_and(|class| class.split_ascii_whitespace().any(|c| c == "checklist"))
        || attribute(&node, "data-type") == Some("checklist")
        || node
            .children()
            .any(|child| attribute(&child, "data-checked").is_some());
    let mut items: Vec<(bool, Vec<Block>)> = Vec::new();
    for child in node.children() {
        match element(&child) {
            Some("li") => {
                let mut blocks = Vec::new();
                read_blocks(child, &mut blocks);
                items.push((is_checked(&child), blocks));
            }
            // A list right inside a list, which some programs write, goes
            // with the item before it.
            Some("ul" | "ol") => {
                let nested = list(child, element(&child) == Some("ol"));
                match items.last_mut() {
                    Some((_, blocks)) => blocks.push(nested),
                    None => items.push((false, vec![nested])),
                }
            }
            _ => {}
        }
    }
    if checklist {
        Block::Checks(items)
    } else if numbered {
        let start = attribute(&node, "start")
            .and_then(|start| start.trim().parse().ok())
            .unwrap_or(1);
        Block::Numbers(start, items.into_iter().map(|(_, blocks)| blocks).collect())
    } else {
        Block::Bullets(items.into_iter().map(|(_, blocks)| blocks).collect())
    }
}

fn is_checked(item: &NodeRef) -> bool {
    if let Some(checked) = attribute(item, "data-checked") {
        return checked == "true";
    }
    attribute(item, "class")
        .is_some_and(|class| class.split_ascii_whitespace().any(|c| c == "checked"))
        || item
            .children()
            .any(|child| element(&child) == Some("input") && attribute(&child, "checked").is_some())
}

fn table(node: NodeRef) -> Block {
    let mut rows = Vec::new();
    collect_rows(node, &mut rows);
    Block::Table(rows)
}

fn collect_rows(node: NodeRef, rows: &mut Vec<Vec<Cell>>) {
    for child in node.children() {
        match element(&child) {
            Some("thead" | "tbody" | "tfoot") => collect_rows(child, rows),
            Some("tr") => {
                let mut cells = Vec::new();
                for cell in child.children() {
                    let header = match element(&cell) {
                        Some("td") => false,
                        Some("th") => true,
                        _ => continue,
                    };
                    let span = |name| {
                        attribute(&cell, name)
                            .and_then(|value| value.trim().parse().ok())
                            .unwrap_or(1)
                    };
                    let colwidth = attribute(&cell, "data-colwidth").and_then(|widths| {
                        widths
                            .split(',')
                            .map(|width| width.trim().parse().ok())
                            .collect::<Option<Vec<u32>>>()
                    });
                    let mut content = Vec::new();
                    read_blocks(cell, &mut content);
                    cells.push(Cell {
                        header,
                        colspan: span("colspan"),
                        rowspan: span("rowspan"),
                        colwidth,
                        content,
                    });
                }
                rows.push(cells);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> Inline {
        Inline::Text(text.into(), Marks::default())
    }

    fn marked(text: &str, marks: Marks) -> Inline {
        Inline::Text(text.into(), marks)
    }

    fn sample() -> Vec<Block> {
        let bold = Marks {
            bold: true,
            ..Marks::default()
        };
        let link = Marks {
            link: Some("https://example.com/?a=1&b=\"2\"".into()),
            ..Marks::default()
        };
        let link_bold = Marks {
            bold: true,
            ..link.clone()
        };
        vec![
            Block::Heading(1, vec![text("Trip <plan> & more")]),
            Block::Paragraph(vec![
                text("Two  spaces,\u{a0}a no-break space, "),
                marked("bold", bold.clone()),
                marked(" and linked bold", link_bold),
                marked(" link", link),
                Inline::Break,
                text(" after a break "),
            ]),
            Block::Paragraph(Vec::new()),
            Block::Quote(vec![Block::Paragraph(vec![text("Quoted")])]),
            Block::Code("fn main() {\n    println!(\"<hi>\");\n}\n".into()),
            Block::Rule,
            Block::Bullets(vec![
                vec![Block::Paragraph(vec![text("One")])],
                vec![
                    Block::Paragraph(vec![text("Two")]),
                    Block::Numbers(3, vec![vec![Block::Paragraph(vec![text("Inner")])]]),
                ],
            ]),
            Block::Checks(vec![
                (true, vec![Block::Paragraph(vec![text("Milk")])]),
                (false, vec![Block::Paragraph(vec![text("Bread")])]),
            ]),
            Block::Table(vec![
                vec![Cell {
                    header: true,
                    colspan: 2,
                    rowspan: 1,
                    colwidth: Some(vec![120, 80]),
                    content: vec![Block::Paragraph(vec![text("Head")])],
                }],
                vec![
                    Cell {
                        header: false,
                        colspan: 1,
                        rowspan: 1,
                        colwidth: None,
                        content: vec![Block::Paragraph(vec![text("A")])],
                    },
                    Cell {
                        header: false,
                        colspan: 1,
                        rowspan: 1,
                        colwidth: None,
                        content: vec![Block::Paragraph(Vec::new())],
                    },
                ],
            ]),
        ]
    }

    #[test]
    fn reads_back_what_it_wrote() {
        let blocks = sample();
        let page = page(&blocks, Some(b"state"));
        assert!(is_document(page.as_bytes()));
        assert_eq!(read(page.as_bytes()), blocks);
        assert_eq!(state_in(page.as_bytes()), Some(b"state".to_vec()));
        assert!(page.contains("<title>Trip &lt;plan&gt; &amp; more</title>"));
    }

    #[test]
    fn a_web_page_isnt_a_document() {
        assert!(!is_document(b"<!DOCTYPE html>\n<html lang=\"en\">"));
        assert_eq!(
            state_in(b"<html><script type=\"application/x-accordion-docs\">{}</script>"),
            None
        );
    }

    #[test]
    fn reads_html_other_programs_write() {
        let html = r#"<html><head><title>x</title><style>p{}</style></head>
            <body>
              <h2 style="color:red" onclick="steal()">Plan</h2>
              <p>
                 Some <b>bold</b> and <i>italic</i>
                 text, <a href="javascript:steal()">a bad link</a>.
              </p>
              <script>steal()</script>
              <div>Loose text<br>
                after a break</div>
              <ul><li>One<ul><li>Nested</li></ul></li><li><p>Two</p></li></ul>
              <img src="x.png" onerror="steal()">
              <table><tr><th>H</th></tr><tr><td>C</td></tr></table>
            </body></html>"#;
        let bold = Marks {
            bold: true,
            ..Marks::default()
        };
        let italic = Marks {
            italic: true,
            ..Marks::default()
        };
        let cell = |header, text_: &str| Cell {
            header,
            colspan: 1,
            rowspan: 1,
            colwidth: None,
            content: vec![Block::Paragraph(vec![text(text_)])],
        };
        assert_eq!(
            read(html.as_bytes()),
            vec![
                Block::Heading(2, vec![text("Plan")]),
                Block::Paragraph(vec![
                    text("Some "),
                    marked("bold", bold),
                    text(" and "),
                    marked("italic", italic),
                    text(" text, a bad link."),
                ]),
                Block::Paragraph(vec![
                    text("Loose text"),
                    Inline::Break,
                    text("after a break")
                ]),
                Block::Bullets(vec![
                    vec![
                        Block::Paragraph(vec![text("One")]),
                        Block::Bullets(vec![vec![Block::Paragraph(vec![text("Nested")])]]),
                    ],
                    vec![Block::Paragraph(vec![text("Two")])],
                ]),
                Block::Table(vec![vec![cell(true, "H")], vec![cell(false, "C")]]),
            ]
        );
    }

    #[test]
    fn an_empty_page_is_one_empty_paragraph() {
        assert_eq!(read(b""), vec![Block::Paragraph(Vec::new())]);
        assert_eq!(read(b"<p>   \n  </p>"), vec![Block::Paragraph(Vec::new())]);
    }
}
