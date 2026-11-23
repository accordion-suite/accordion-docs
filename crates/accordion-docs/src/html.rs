//! A document's file: an ordinary HTML page that any browser shows, with
//! the co-editing state in a data block browsers neither run nor show
//! (decision 30).
//!
//! The server writes the body from the schema in [`crate::model`], so only
//! its elements and attributes reach the file: never a script or an event
//! handler, and styles only as the schema writes them. Reading goes the other way, through the
//! same schema, for HTML Docs wrote and HTML any other program wrote.
//!
//! The writer puts each block on a line of its own, and nothing else between
//! a block's tags, so the reader gets back exactly what was written.
//! Spaces typed inside text are kept as typed. Only text with line breaks or
//! tabs in it, which is how other programs lay out their HTML, has its
//! whitespace folded, as a browser shows it.

use std::collections::HashMap;
use std::fmt::Write as _;

use scraper::{Html, Node};

use crate::fonts;
use crate::model::{
    self, Align, Block, Cell, ContentsStyle, Field, Image, Inline, Kind, ListStyle, Marks,
    PageSetup, PageSize, Para, Parts, Row, SIDES, Script,
};
use crate::office::number;
use crate::picture::{self, Picture, Pictures};
use crate::style::{self, Color};

/// How every document starts. Docs recognizes its files by it, so a web page
/// someone uploads stays a web page.
pub const MARKER: &str = "<!DOCTYPE html>\n<html data-accordion-docs=\"1\"";

/// The data block's type. Browsers skip scripts of a type they don't know.
const DATA_TYPE: &str = "application/x-accordion-docs";

/// The format version in the data block.
pub const DATA_VERSION: u32 = 1;

/// How the page looks on screen and in print, kept small. The editor in the
/// web app has its own styles. Only blocks of text keep typed spaces: list
/// items and cells hold blocks, with line breaks between them in the file
/// that aren't part of the text. Text is Arimo at 11 points, as Google Docs
/// uses Arial, or Arial where Arimo isn't installed, since the two have the
/// same letter widths. Lines and
/// paragraphs are spaced as the Word download spaces them: 1.15 lines, 6
/// points after each paragraph, and Google Docs' space around headings.
/// Headings have Google Docs' look too: regular weight, the smaller ones
/// gray.
const STYLE: &str = "\
body{margin:0;background:#f8f9fa;color:#1f1f1f;font:11pt/1.3225 Arimo,Arial,'Liberation Sans',sans-serif}\
main{box-sizing:border-box;max-width:816px;margin:24px auto;padding:72px 96px;background:#fff;\
box-shadow:0 1px 3px rgba(0,0,0,.15)}\
p,h1,h2,h3,h4,h5,h6{white-space:pre-wrap}\
p{margin:0 0 6pt}\
h1,h2,h3,h4,h5,h6{font-weight:400}\
h1{font-size:20pt;margin:20pt 0 6pt}h2{font-size:16pt;margin:18pt 0 6pt}h3{font-size:14pt;color:#434343;margin:16pt 0 4pt}\
h4{font-size:12pt;color:#666;margin:14pt 0 4pt}h5{font-size:11pt;color:#666;margin:12pt 0 4pt}\
h6{font-size:11pt;color:#666;font-style:italic;margin:12pt 0 4pt}\
p.title{font-size:26pt;margin:0 0 3pt}p.subtitle{font-size:15pt;color:#666;margin:0 0 16pt}\
main>:first-child{margin-top:0}\
ul[data-list=disc]{list-style-type:disc}ul[data-list=circle]{list-style-type:circle}\
ul[data-list=square]{list-style-type:square}ul[data-list=dash]{list-style-type:'\\2013  '}\
ul[data-list=arrow]{list-style-type:'\\27A2  '}ul[data-list=star]{list-style-type:'\\2605  '}\
@counter-style decimal-paren{system:extends decimal;suffix:') '}\
ol[data-list=decimal]{list-style-type:decimal}ol[data-list=decimal-paren]{list-style-type:decimal-paren}\
ol[data-list=lower-alpha]{list-style-type:lower-alpha}ol[data-list=upper-alpha]{list-style-type:upper-alpha}\
ol[data-list=lower-roman]{list-style-type:lower-roman}ol[data-list=upper-roman]{list-style-type:upper-roman}\
div.page-break{break-after:page}\
@media screen{div.page-break{margin:12pt 0;border-top:1px dashed #c4c7c5}}\
pre{background:#f1f3f4;padding:12px;border-radius:4px;overflow-x:auto;white-space:pre-wrap}\
code{font-family:ui-monospace,monospace;font-size:.9em}\
img{display:block;max-width:100%;height:auto;margin:0 0 .6em}\
blockquote{margin:0 0 .6em;padding-left:16px;border-left:3px solid #dadce0;color:#444746}\
hr{margin:.5em 0;border:none;border-top:1px solid #c4c7c5}\
table{border-collapse:collapse;margin:0 0 .6em}\
td,th{border:1px solid #c4c7c5;padding:4px 8px;vertical-align:top;min-width:40px}\
th{background:#f1f3f4;text-align:left}\
td>p:last-child,th>p:last-child,li>p:last-child{margin:0}\
ul.checklist{list-style:none;padding-left:4px}\
ul.checklist>li>input{float:left;margin:6px 8px 0 0}\
ul.checklist>li[data-checked=true]>p{text-decoration:line-through;color:#5f6368}\
header[data-part]{margin-bottom:18pt}footer[data-part]{margin-top:18pt}\
a[data-footnote-ref]{text-decoration:none;color:inherit}\
nav[data-contents]{margin:0 0 12pt}nav[data-contents]>p{margin:0 0 4pt}\
nav[data-contents]>p>a{color:inherit;text-decoration:none}nav[data-contents=links]>p>a{color:#1155cc;text-decoration:underline}\
.contents-2{margin-left:18pt}.contents-3{margin-left:36pt}.contents-4{margin-left:54pt}\
.contents-5{margin-left:72pt}.contents-6{margin-left:90pt}\
section[data-footnotes]{margin-top:18pt;padding-top:6pt;border-top:1px solid #c4c7c5;font-size:10pt}\
section[data-footnotes]>ol{margin:0;padding-left:2em}section[data-footnotes] p{margin:0}\
main[data-first-page=different]>header[data-part=header],footer[data-part=first-footer]{display:none}\
@media (max-width:860px){main{margin:0;padding:24px 16px;box-shadow:none}}\
@media print{body{background:none}main{max-width:none;margin:0;padding:0;box-shadow:none}}";

/// Dark colors, on screens only. A document with colored text or
/// highlights stays as it was written, dark on white, so its colors still
/// read.
const DARK_STYLE: &str = "@media screen and (prefers-color-scheme:dark){body{background:#131314;color:#e3e3e3}\
main{background:#1f1f1f}pre,th{background:#2d2f31}td,th,hr,section[data-footnotes]{border-color:#444746}h3{color:#c4c7c5}\
h4,h5,h6,p.subtitle{color:#9aa0a6}\
blockquote{border-color:#444746;color:#c4c7c5}nav[data-contents=links]>p>a{color:#8ab4f8}}";

/// The whole file for `blocks` on the page `setup`, with `state`, the
/// co-editing state, in its data block when there is one.
pub fn page(
    blocks: &[Block],
    parts: &Parts,
    setup: Option<&PageSetup>,
    state: Option<&[u8]>,
    pictures: &Pictures,
) -> String {
    let title = model::first_text(blocks)
        .map(|text| text.chars().take(100).collect::<String>())
        .unwrap_or_else(|| "Untitled document".to_owned());
    write_page(blocks, parts, setup, state, &title, pictures)
}

fn write_page(
    blocks: &[Block],
    parts: &Parts,
    setup: Option<&PageSetup>,
    state: Option<&[u8]>,
    title: &str,
    pictures: &Pictures,
) -> String {
    let room: usize = pictures
        .values()
        .map(|picture| picture.contents.len() / 3 * 4 + 100)
        .sum();
    let mut out = String::with_capacity(4096 + room);
    out.push_str(MARKER);
    out.push_str(">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<meta name=\"generator\" content=\"Accordion Docs\">\n");
    let _ = writeln!(out, "<title>{}</title>", escape_text(title));
    let colored = std::iter::once(blocks)
        .chain(parts.each().map(|(_, part)| part))
        .any(|blocks| {
            model::any_marks(blocks, &|marks| {
                marks.color.is_some() || marks.highlight.is_some()
            }) || model::any_block_color(blocks)
        });
    let _ = writeln!(
        out,
        "<style>{STYLE}{}</style>",
        if colored { "" } else { DARK_STYLE }
    );
    let first = if parts.different_first {
        " data-first-page=\"different\""
    } else {
        ""
    };
    match setup {
        Some(setup) => {
            let _ = writeln!(out, "<style>{}</style>", page_style(setup));
            out.push_str("</head>\n<body>\n");
            let _ = writeln!(
                out,
                "<main data-page=\"{}\" data-orientation=\"{}\" data-margins=\"{}\"{first}>",
                setup.size.name(),
                if setup.landscape {
                    "landscape"
                } else {
                    "portrait"
                },
                setup
                    .margins
                    .iter()
                    .map(|margin| number(*margin))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        None => {
            let _ = writeln!(out, "</head>\n<body>\n<main{first}>");
        }
    }
    // Headers before the text and footers after it, as a browser shows the
    // file on screen. Printing from Docs puts them on every page.
    let part = |out: &mut String, tag: &str, name: &str, blocks: &[Block]| {
        if blocks.is_empty() {
            return;
        }
        let _ = writeln!(out, "<{tag} data-part=\"{}\">", name.replace('_', "-"));
        // Headers and footers have no footnotes.
        write_blocks(out, blocks, pictures, &mut Notes::default());
        let _ = writeln!(out, "</{tag}>");
    };
    part(&mut out, "header", "first_header", &parts.first_header);
    part(&mut out, "header", "header", &parts.header);
    let mut notes = Notes::for_document(blocks);
    write_blocks(&mut out, blocks, pictures, &mut notes);
    notes.write_section(&mut out);
    part(&mut out, "footer", "footer", &parts.footer);
    part(&mut out, "footer", "first_footer", &parts.first_footer);
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

/// The page's size and margins: in print, as the paper's, and on a wide
/// screen, as the page's width and padding. Phones keep the narrow layout.
fn page_style(setup: &PageSetup) -> String {
    let (width, height) = setup.css_size();
    let [top, right, bottom, left] = setup.margins.map(number);
    format!(
        "@page{{size:{width} {height};margin:{top}pt {right}pt {bottom}pt {left}pt}}\
         @media screen and (min-width:861px){{main{{max-width:{width};\
         padding:{top}pt {right}pt {bottom}pt {left}pt}}}}"
    )
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

/// `blocks` as HTML on their own, without a page around them, as Markdown
/// carries what it has no syntax for.
pub(crate) fn fragment(blocks: &[Block], pictures: &Pictures, notes: &mut Notes) -> String {
    let mut out = String::new();
    write_blocks(&mut out, blocks, pictures, notes);
    out
}

/// The footnotes written so far, each one's text in the order they're
/// numbered, and, in a document with a table of contents, the headings it
/// links to.
#[derive(Default)]
pub(crate) struct Notes {
    pub(crate) texts: Vec<Vec<Inline>>,
    /// Each heading's anchor, in order, while the document has a table of
    /// contents, and how many headings are written.
    anchors: Vec<Option<String>>,
    headings: usize,
    /// The table of contents' entries.
    pub(crate) contents: Vec<model::Entry>,
}

impl Notes {
    /// For writing `blocks`, a whole document's text.
    pub(crate) fn for_document(blocks: &[Block]) -> Self {
        if !model::has_contents(blocks) {
            return Self::default();
        }
        Self {
            anchors: model::heading_anchors(blocks),
            contents: model::contents(blocks),
            ..Self::default()
        }
    }

    /// The next heading's anchor, if a table of contents links to it.
    pub(crate) fn next_anchor(&mut self) -> Option<String> {
        let anchor = self.anchors.get(self.headings).cloned().flatten();
        self.headings += 1;
        anchor
    }

    /// Takes in a footnote's text, and returns its number.
    pub(crate) fn add(&mut self, text: &[Inline]) -> usize {
        self.texts.push(text.to_vec());
        self.texts.len()
    }

    /// The notes at the end of the text, as GitHub writes them, so pages
    /// that know its footnotes read them as footnotes too.
    fn write_section(&self, out: &mut String) {
        if self.texts.is_empty() {
            return;
        }
        out.push_str("<section class=\"footnotes\" data-footnotes>\n<ol>\n");
        for (index, text) in self.texts.iter().enumerate() {
            let _ = write!(out, "<li id=\"fn-{}\"><p>", index + 1);
            write_inlines(out, text, &mut Notes::default());
            out.push_str("</p></li>\n");
        }
        out.push_str("</ol>\n</section>\n");
    }
}

fn write_blocks(out: &mut String, blocks: &[Block], pictures: &Pictures, notes: &mut Notes) {
    for block in blocks {
        write_block(out, block, pictures, notes);
    }
}

/// A paragraph's or heading's opening tag, with its layout.
fn open_para(out: &mut String, tag: &str, para: &Para, id: Option<&str>) {
    let _ = write!(out, "<{tag}");
    if let Some(id) = id {
        let _ = write!(out, " id=\"{}\"", escape_attribute(id));
    }
    if let Some(kind) = para.kind {
        let _ = write!(out, " class=\"{}\"", kind.name());
    }
    let css = para_style(para);
    if !css.is_empty() {
        let _ = write!(out, " style=\"{css}\"");
    }
    // Line spacing in Docs' own terms, since the style's line height is
    // one other programs read differently.
    if let Some(line) = para.line {
        let _ = write!(
            out,
            " data-line-height=\"{}\"",
            number(f64::from(line) / 100.0)
        );
    }
    out.push('>');
}

/// The CSS for a paragraph's layout, such as `text-align:center`. Every
/// part is a number or a name from the schema, so none can end the
/// attribute.
pub(crate) fn para_style(para: &Para) -> String {
    let mut parts = Vec::new();
    if para.align != Align::Left {
        parts.push(format!("text-align:{}", para.align.name()));
    }
    let length = |twips| format!("{}pt", number(model::points(twips)));
    if para.indent != 0 {
        parts.push(format!("margin-left:{}", length(para.indent)));
    }
    if para.indent_right != 0 {
        parts.push(format!("margin-right:{}", length(para.indent_right)));
    }
    if para.first_line != 0 {
        parts.push(format!("text-indent:{}", length(para.first_line)));
    }
    if let Some(line) = para.line {
        parts.push(format!(
            "line-height:{}",
            number(f64::from(line) / 100.0 * model::SINGLE)
        ));
    }
    if let Some(before) = para.before {
        parts.push(format!("margin-top:{}", length(before)));
    }
    if let Some(after) = para.after {
        parts.push(format!("margin-bottom:{}", length(after)));
    }
    parts.join(";")
}

/// A list's opening tag, with its style: a name Docs reads back, and the
/// old `type` attribute that programs without styles still show.
fn open_list(out: &mut String, tag: &str, style: Option<ListStyle>, start: Option<u32>) {
    let _ = write!(out, "<{tag}");
    if let Some(start) = start.filter(|start| *start != 1) {
        let _ = write!(out, " start=\"{start}\"");
    }
    if let Some(style) = style {
        let kind = match style {
            ListStyle::LowerAlpha => Some("a"),
            ListStyle::UpperAlpha => Some("A"),
            ListStyle::LowerRoman => Some("i"),
            ListStyle::UpperRoman => Some("I"),
            _ => None,
        };
        if let Some(kind) = kind {
            let _ = write!(out, " type=\"{kind}\"");
        }
        let _ = write!(out, " data-list=\"{}\"", style.name());
    }
    out.push_str(">\n");
}

fn write_block(out: &mut String, block: &Block, pictures: &Pictures, notes: &mut Notes) {
    match block {
        Block::Paragraph(para, inlines) => {
            open_para(out, "p", para, None);
            write_inlines(out, inlines, notes);
            out.push_str("</p>\n");
        }
        Block::Heading(level, para, inlines) => {
            let id = notes.next_anchor();
            open_para(out, &format!("h{level}"), para, id.as_deref());
            write_inlines(out, inlines, notes);
            let _ = writeln!(out, "</h{level}>");
        }
        Block::PageBreak => out.push_str("<div class=\"page-break\"></div>\n"),
        Block::Contents(style) => write_contents(out, *style, &notes.contents),
        Block::Quote(blocks) => {
            out.push_str("<blockquote>\n");
            write_blocks(out, blocks, pictures, notes);
            out.push_str("</blockquote>\n");
        }
        Block::Code(text) => {
            let _ = writeln!(out, "<pre><code>{}</code></pre>", escape_text(text));
        }
        Block::Rule(rule) => {
            if rule.thickness.is_none() && rule.color.is_none() {
                out.push_str("<hr>\n");
            } else {
                let _ = writeln!(out, "<hr style=\"{}\">", rule_style(rule));
            }
        }
        Block::Image(image) => write_image(out, image, pictures.get(&image.hash)),
        Block::Bullets(style, items) => {
            open_list(out, "ul", *style, None);
            for blocks in items {
                out.push_str("<li>\n");
                write_blocks(out, blocks, pictures, notes);
                out.push_str("</li>\n");
            }
            out.push_str("</ul>\n");
        }
        Block::Numbers(start, style, items) => {
            open_list(out, "ol", *style, Some(*start));
            for blocks in items {
                out.push_str("<li>\n");
                write_blocks(out, blocks, pictures, notes);
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
                write_blocks(out, blocks, pictures, notes);
                out.push_str("</li>\n");
            }
            out.push_str("</ul>\n");
        }
        Block::Table(rows) => {
            out.push_str("<table>\n<tbody>\n");
            for row in rows {
                match row.height {
                    Some(height) => {
                        let _ = writeln!(
                            out,
                            "<tr style=\"height:{}pt\">",
                            number(model::points(height))
                        );
                    }
                    None => out.push_str("<tr>\n"),
                }
                for cell in &row.cells {
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
                    let css = cell_style(cell);
                    if !css.is_empty() {
                        let _ = write!(out, " style=\"{css}\"");
                    }
                    out.push_str(">\n");
                    write_blocks(out, &cell.content, pictures, notes);
                    let _ = writeln!(out, "</{tag}>");
                }
                out.push_str("</tr>\n");
            }
            out.push_str("</tbody>\n</table>\n");
        }
    }
}

/// A table of contents: a link to each heading, indented by its level.
/// Page numbers are left out, since a web page has no pages.
fn write_contents(out: &mut String, style: ContentsStyle, entries: &[model::Entry]) {
    let _ = writeln!(
        out,
        "<nav class=\"contents\" data-contents=\"{}\">",
        style.name()
    );
    for entry in entries {
        let _ = writeln!(
            out,
            "<p class=\"contents-{}\"><a href=\"#{}\">{}</a></p>",
            entry.level,
            escape_attribute(&entry.anchor),
            escape_text(&entry.text)
        );
    }
    out.push_str("</nav>\n");
}

/// The CSS for a line with a thickness or color of its own.
fn rule_style(rule: &model::Rule) -> String {
    let thickness = rule.thickness.map_or("1px".to_owned(), |thickness| {
        format!("{}pt", number(model::points(thickness)))
    });
    let color = rule.color.map_or("#c4c7c5".to_owned(), Color::css);
    format!("border:none;border-top:{thickness} solid {color}")
}

/// The CSS for a cell's color, borders, and padding. Each part is a number
/// or a color from the schema, so none can end the attribute.
fn cell_style(cell: &Cell) -> String {
    let mut parts = Vec::new();
    if let Some(color) = cell.background {
        parts.push(format!("background-color:{}", color.css()));
    }
    let color = cell.border_color.unwrap_or(Color::BLACK).css();
    for (side, border) in SIDES.iter().zip(cell.borders) {
        match border {
            Some(0) => parts.push(format!("border-{side}:none")),
            Some(border) => parts.push(format!(
                "border-{side}:{}pt solid {color}",
                number(model::points(border))
            )),
            None => {}
        }
    }
    if let Some(padding) = cell.padding {
        let sides: Vec<String> = padding
            .iter()
            .map(|side| format!("{}pt", number(model::points(*side))))
            .collect();
        parts.push(format!("padding:{}", sides.join(" ")));
    }
    parts.join(";")
}

/// A picture, with its contents in its `src`. One whose contents are
/// missing keeps its hash, so it comes back if they turn up.
fn write_image(out: &mut String, image: &Image, picture: Option<&Picture>) {
    out.push_str("<img");
    if let Some(picture) = picture {
        let _ = write!(out, " src=\"{}\"", picture.data_address());
    }
    let _ = write!(
        out,
        " alt=\"{}\" data-hash=\"{}\"",
        escape_attribute(&image.alt),
        image.hash
    );
    if let Some(width) = image.width {
        let _ = write!(out, " width=\"{width}\"");
    }
    out.push_str(">\n");
}

/// A mark as a tag, in the order they nest, outermost first.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Tag {
    Link(String),
    /// Color, highlight, size, and font, as one `<span style>`.
    Style(String),
    /// A weight other than regular and bold, with whether it's bold too,
    /// inside `<strong>` as the editor writes it.
    Weight(u16, bool),
    Bold,
    Italic,
    Underline,
    Strike,
    Code,
    Super,
    Sub,
    /// A page number or count, around its stand-in text.
    Field(Field),
}

/// The CSS for a run's styles, such as `color:#ff0000;font-size:12pt`.
/// [`Marks::normalized`] has checked each part, so none can end the
/// attribute.
pub(crate) fn style_of(marks: &Marks) -> String {
    let mut parts = Vec::new();
    if let Some(font) = &marks.font {
        // The font made to the same widths after it, for a browser outside
        // Docs that doesn't have the font, such as Carlito's Calibri.
        match fonts::twin(font) {
            Some(twin) => parts.push(format!("font-family:'{font}','{twin}'")),
            None => parts.push(format!("font-family:'{font}'")),
        }
    }
    if let Some(size) = marks.size {
        parts.push(format!("font-size:{}pt", style::points_text(size)));
    }
    if let Some(color) = marks.color {
        parts.push(format!("color:{}", color.css()));
    }
    if let Some(color) = marks.highlight {
        parts.push(format!("background-color:{}", color.css()));
    }
    parts.join(";")
}

impl Tag {
    fn of(marks: &Marks) -> Vec<Tag> {
        let mut tags = Vec::new();
        if let Some(href) = &marks.link {
            tags.push(Tag::Link(href.clone()));
        }
        if marks.has_style() {
            tags.push(Tag::Style(style_of(marks)));
        }
        for (on, tag) in [
            (marks.bold, Tag::Bold),
            (marks.italic, Tag::Italic),
            (marks.underline, Tag::Underline),
            (marks.strike, Tag::Strike),
            (marks.code, Tag::Code),
            (marks.script == Some(Script::Super), Tag::Super),
            (marks.script == Some(Script::Sub), Tag::Sub),
        ] {
            if on {
                tags.push(tag);
            }
        }
        if let Some(weight) = marks.weight {
            tags.push(Tag::Weight(weight, marks.bold));
        }
        if let Some(field) = marks.field {
            tags.push(Tag::Field(field));
        }
        tags
    }

    fn open(&self, out: &mut String) {
        match self {
            Tag::Link(href) => {
                let _ = write!(out, "<a href=\"{}\">", escape_attribute(href));
            }
            Tag::Style(css) => {
                let _ = write!(out, "<span style=\"{}\">", escape_attribute(css));
            }
            Tag::Weight(weight, bold) => {
                // The weight it shows, for browsers, and its own, for Docs.
                let shown = fonts::shown_weight(Some(*weight), *bold);
                let _ = write!(
                    out,
                    "<span data-weight=\"{weight}\" style=\"font-weight:{shown}\">"
                );
            }
            Tag::Bold => out.push_str("<strong>"),
            Tag::Italic => out.push_str("<em>"),
            Tag::Underline => out.push_str("<u>"),
            Tag::Strike => out.push_str("<s>"),
            Tag::Code => out.push_str("<code>"),
            Tag::Super => out.push_str("<sup>"),
            Tag::Sub => out.push_str("<sub>"),
            Tag::Field(field) => {
                let _ = write!(out, "<span data-field=\"{}\">", field.name());
            }
        }
    }

    fn close(&self, out: &mut String) {
        out.push_str(match self {
            Tag::Link(_) => "</a>",
            Tag::Style(_) | Tag::Weight(..) => "</span>",
            Tag::Bold => "</strong>",
            Tag::Italic => "</em>",
            Tag::Underline => "</u>",
            Tag::Strike => "</s>",
            Tag::Code => "</code>",
            Tag::Super => "</sup>",
            Tag::Sub => "</sub>",
            Tag::Field(_) => "</span>",
        });
    }
}

/// Text with its marks as HTML, without a block around it.
pub(crate) fn inline_fragment(inlines: &[Inline], notes: &mut Notes) -> String {
    let mut out = String::new();
    write_inlines(&mut out, inlines, notes);
    out
}

/// Text with its marks, keeping tags open across runs that share them.
fn write_inlines(out: &mut String, inlines: &[Inline], notes: &mut Notes) {
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
            Inline::Footnote(text) => {
                // A link can't hold the footnote's own link, so marks close
                // around it.
                while let Some(tag) = open.pop() {
                    tag.close(out);
                }
                let number = notes.add(text);
                let _ = write!(
                    out,
                    "<sup><a href=\"#fn-{number}\" id=\"fnref-{number}\" data-footnote-ref>{number}</a></sup>"
                );
            }
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

pub(crate) fn escape_attribute(text: &str) -> String {
    escape_text(text).replace('"', "&quot;")
}

/// The blocks of an HTML page, through Docs' schema. Everything the schema
/// doesn't have is left out: scripts, styles, forms, and so on. Text in
/// elements the schema doesn't know stays, as plain text.
pub fn read(contents: &[u8]) -> Vec<Block> {
    read_document(contents).blocks
}

/// A page as Docs reads it.
#[derive(Debug, Default)]
pub struct Document {
    pub blocks: Vec<Block>,
    /// The page setup Docs wrote on its `<main>`, if it has one.
    pub setup: Option<PageSetup>,
    /// The pictures in its body, by their hash.
    pub pictures: Pictures,
    /// Its headers and footers.
    pub parts: Parts,
}

/// The blocks of an HTML page, as [`read`] reads them, with its page setup
/// and its pictures.
pub fn read_document(contents: &[u8]) -> Document {
    let text = String::from_utf8_lossy(contents);
    let html = Html::parse_document(&text);
    let root = html.root_element();
    // Docs writes its blocks in <main>. Other pages are read whole.
    let main = root
        .descendants()
        .find(|node| matches!(node.value(), Node::Element(el) if el.name() == "main"));
    let mut blocks = Vec::new();
    let mut pictures = Pictures::new();
    let notes = note_texts(*root);
    let setup = main.and_then(|main| read_setup(&main));
    let mut parts = Parts::default();
    match main {
        Some(main) => {
            read_blocks(main, &mut blocks, &mut pictures, &notes);
            parts.different_first = attribute(&main, "data-first-page") == Some("different");
            for child in main.children() {
                let Some(name) = part_name(&child) else {
                    continue;
                };
                let part = match name {
                    "header" => &mut parts.header,
                    "footer" => &mut parts.footer,
                    "first-header" => &mut parts.first_header,
                    "first-footer" => &mut parts.first_footer,
                    _ => continue,
                };
                read_blocks(child, part, &mut pictures, &notes);
            }
        }
        None => read_blocks(*root, &mut blocks, &mut pictures, &notes),
    }
    let blocks = model::normalize(blocks);
    let parts = parts.normalized();
    // Only pictures the document shows.
    let mut shown = model::image_hashes(&blocks);
    for (_, part) in parts.each() {
        shown.extend(model::image_hashes(part));
    }
    pictures.retain(|hash, _| shown.contains(hash));
    Document {
        blocks,
        setup,
        pictures,
        parts,
    }
}

/// The part a `<header data-part>` or `<footer data-part>` holds, such as
/// `first-header`.
fn part_name<'a>(node: &NodeRef<'a>) -> Option<&'a str> {
    match node.value() {
        Node::Element(el) if matches!(el.name(), "header" | "footer") => el.attr("data-part"),
        _ => None,
    }
}

/// The page setup on Docs' `<main>`. A size Docs doesn't know means no
/// setup; margins that are missing or unreadable are an inch.
fn read_setup(main: &NodeRef) -> Option<PageSetup> {
    let size = PageSize::from_name(attribute(main, "data-page")?.trim())?;
    let landscape = attribute(main, "data-orientation").map(str::trim) == Some("landscape");
    let mut margins = [model::DEFAULT_MARGIN; 4];
    if let Some(written) = attribute(main, "data-margins") {
        let read: Vec<f64> = written
            .split_ascii_whitespace()
            .filter_map(|margin| margin.parse().ok())
            .collect();
        if let Ok(read) = <[f64; 4]>::try_from(read) {
            margins = read;
        }
    }
    Some(
        PageSetup {
            size,
            landscape,
            margins,
        }
        .normalized(),
    )
}

type NodeRef<'a> = ego_tree::NodeRef<'a, Node>;

/// Elements whose contents are never part of a document.
const SKIPPED: &[&str] = &[
    "head", "script", "style", "template", "noscript", "iframe", "object", "embed", "svg", "math",
    "title", "meta", "link", "input", "button", "select", "textarea", "video", "audio", "canvas",
    "picture", "source", "track", "map", "area", "dialog",
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
    Note(Vec<Inline>),
}

impl Runs {
    fn is_blank(&self) -> bool {
        self.runs.iter().all(|run| match run {
            Run::Text { text, folded, .. } => *folded && text.trim().is_empty(),
            Run::Break | Run::Note(_) => false,
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
                    Run::Note(text) => Inline::Footnote(text),
                })
                .collect(),
        )
    }
}

/// Footnotes' text, by the `id` their numbers link to.
type NoteTexts = HashMap<String, Vec<Inline>>;

/// Whether `node` is the notes at the end of a page, as Docs and GitHub
/// write them.
fn is_notes(node: &NodeRef) -> bool {
    attribute(node, "data-footnotes").is_some()
}

/// The text of each note in a page's notes, by its `id`. Each paragraph in a
/// note is a line of its own.
fn note_texts(root: NodeRef) -> NoteTexts {
    let mut notes = NoteTexts::new();
    let empty = NoteTexts::new();
    for section in root.descendants().filter(is_notes) {
        for item in section.descendants() {
            if element(&item) != Some("li") {
                continue;
            }
            let Some(id) = attribute(&item, "id") else {
                continue;
            };
            let mut text = Vec::new();
            let mut runs = Runs::default();
            for child in item.children() {
                if matches!(element(&child), Some("p" | "div")) {
                    let mut line = inlines_of(child, &empty);
                    // The space before a link back to the number, which
                    // is left out.
                    if let Some(Inline::Text(last, _)) = line.last_mut() {
                        last.truncate(last.trim_end().len());
                    }
                    line = model::normalize_inlines(line);
                    if !text.is_empty() && !line.is_empty() {
                        text.push(Inline::Break);
                    }
                    text.extend(line);
                } else {
                    read_inline(child, &Marks::default(), &mut runs, &empty);
                }
            }
            let loose = runs.finish();
            if !text.is_empty() && !loose.is_empty() {
                text.push(Inline::Break);
            }
            text.extend(loose);
            notes.entry(id.to_owned()).or_insert(text);
        }
    }
    notes
}

/// The note a footnote's number links to: its own `href`, or a link's
/// inside it.
fn note_of<'a>(node: NodeRef, notes: &'a NoteTexts) -> Option<&'a Vec<Inline>> {
    std::iter::once(node)
        .chain(node.descendants())
        .filter_map(|node| attribute(&node, "href"))
        .find_map(|href| notes.get(href.strip_prefix('#')?))
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
        "sup",
        "sub",
        "mark",
        "span",
        "font",
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
        "img",
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
fn read_blocks(parent: NodeRef, out: &mut Vec<Block>, pictures: &mut Pictures, notes: &NoteTexts) {
    let mut runs = Runs::default();
    let flush = |runs: &mut Runs, out: &mut Vec<Block>| {
        let taken = std::mem::take(runs);
        if !taken.is_blank() {
            out.push(Block::Paragraph(Para::default(), taken.finish()));
        }
    };
    for child in parent.children() {
        let name = match child.value() {
            Node::Text(text) => {
                add_text(&mut runs, &text[..], &Marks::default());
                continue;
            }
            Node::Element(_) if part_name(&child).is_some() || is_notes(&child) => continue,
            Node::Element(_) => element(&child).unwrap_or("other"),
            _ => continue,
        };
        if SKIPPED.contains(&name) {
            continue;
        }
        let (break_before, break_after) = page_breaks(&child);
        if break_before {
            flush(&mut runs, out);
            out.push(Block::PageBreak);
            // Google Docs and Word mark a break with a rule or a line
            // break that isn't shown.
            if matches!(name, "hr" | "br") {
                continue;
            }
        }
        match name {
            "p" => {
                flush(&mut runs, out);
                let inlines = inlines_of(child, notes);
                // Google Docs' line is a picture in a paragraph of its own,
                // which becomes the line alone.
                if !(inlines.is_empty() && has_line(child)) {
                    out.push(Block::Paragraph(para_of(&child), inlines));
                }
                images_in(child, out, pictures);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                flush(&mut runs, out);
                let level = name[1..].parse().unwrap_or(1);
                let inlines = inlines_of(child, notes);
                if !(inlines.is_empty() && has_line(child)) {
                    out.push(Block::Heading(level, para_of(&child), inlines));
                }
                images_in(child, out, pictures);
            }
            // Its entries come from the headings, so they aren't read.
            "nav" if attribute(&child, "data-contents").is_some() => {
                flush(&mut runs, out);
                out.push(Block::Contents(
                    attribute(&child, "data-contents")
                        .and_then(ContentsStyle::from_name)
                        .unwrap_or_default(),
                ));
            }
            "div" if has_class(&child, "page-break") => {
                flush(&mut runs, out);
                if !break_after {
                    out.push(Block::PageBreak);
                }
            }
            "img" => {
                flush(&mut runs, out);
                out.extend(google_line(child).or_else(|| image(child, pictures)));
            }
            "blockquote" => {
                flush(&mut runs, out);
                let mut blocks = Vec::new();
                read_blocks(child, &mut blocks, pictures, notes);
                out.push(Block::Quote(blocks));
            }
            "pre" => {
                flush(&mut runs, out);
                out.push(Block::Code(text_of(child)));
            }
            "hr" => {
                flush(&mut runs, out);
                out.push(Block::Rule(rule_of(&child)));
            }
            "ul" | "ol" => {
                flush(&mut runs, out);
                out.push(list(child, name == "ol", pictures, notes));
            }
            "table" => {
                flush(&mut runs, out);
                out.push(table(child, pictures, notes));
            }
            "br" => runs.runs.push(Run::Break),
            "div" | "section" | "article" | "main" | "header" | "footer" | "nav" | "aside"
            | "figure" | "figcaption" | "body" | "html" | "center" | "form" | "fieldset"
            | "details" | "summary" | "address" | "dl" | "dd" | "dt" | "li" | "thead" | "tbody"
            | "tfoot" | "tr" | "td" | "th" | "caption" => {
                flush(&mut runs, out);
                read_blocks(child, out, pictures, notes);
            }
            "colgroup" | "col" => {}
            // Text elements around blocks, as Google Docs wraps what it
            // copies in a <b>, are read as a <div> would be.
            _ if holds_blocks(&child) => {
                flush(&mut runs, out);
                read_blocks(child, out, pictures, notes);
            }
            _ => read_inline(child, &Marks::default(), &mut runs, notes),
        }
        if break_after {
            flush(&mut runs, out);
            out.push(Block::PageBreak);
        }
    }
    flush(&mut runs, out);
}

/// Whether any of an element's children is a block.
fn holds_blocks(node: &NodeRef) -> bool {
    node.children().any(|child| {
        matches!(
            element(&child),
            Some(
                "p" | "h1"
                    | "h2"
                    | "h3"
                    | "h4"
                    | "h5"
                    | "h6"
                    | "blockquote"
                    | "pre"
                    | "hr"
                    | "ul"
                    | "ol"
                    | "table"
                    | "div"
            )
        )
    })
}

fn has_class(node: &NodeRef, name: &str) -> bool {
    attribute(node, "class").is_some_and(|class| {
        class
            .split_ascii_whitespace()
            .any(|class| class.eq_ignore_ascii_case(name))
    })
}

/// Whether an element's style starts a new page before it, and after it.
fn page_breaks(node: &NodeRef) -> (bool, bool) {
    let Some(css) = attribute(node, "style") else {
        return (false, false);
    };
    let mut breaks = (false, false);
    for (name, value) in style::declarations(css) {
        let value = value.trim().to_ascii_lowercase();
        let starts_page = matches!(
            value.as_str(),
            "always" | "page" | "left" | "right" | "recto" | "verso"
        );
        match name.as_str() {
            "page-break-before" | "break-before" => breaks.0 = starts_page,
            "page-break-after" | "break-after" => breaks.1 = starts_page,
            _ => {}
        }
    }
    breaks
}

/// A paragraph's or heading's layout, from its style, its class (Docs',
/// Google Docs', and Word's names for titles and subtitles), and the old
/// `align` attribute.
fn para_of(node: &NodeRef) -> Para {
    let mut para = Para::default();
    if has_class(node, "title") || has_class(node, "MsoTitle") {
        para.kind = Some(Kind::Title);
    } else if has_class(node, "subtitle") || has_class(node, "MsoSubtitle") {
        para.kind = Some(Kind::Subtitle);
    }
    if let Some(align) = attribute(node, "align").and_then(Align::from_name) {
        para.align = align;
    }
    if let Some(css) = attribute(node, "style") {
        for (name, value) in style::declarations(css) {
            let length = |text: &str| style::length_from_css(text).and_then(model::twips);
            match name.as_str() {
                "text-align" => {
                    if let Some(align) = Align::from_name(&value) {
                        para.align = align;
                    }
                }
                "margin-left" | "margin-inline-start" => {
                    para.indent = length(&value).unwrap_or(para.indent);
                }
                "margin-right" | "margin-inline-end" => {
                    para.indent_right = length(&value).unwrap_or(para.indent_right);
                }
                "text-indent" => para.first_line = length(&value).unwrap_or(para.first_line),
                "line-height" => para.line = style::line_from_css(&value).or(para.line),
                "margin-top" | "margin-block-start" => para.before = length(&value).or(para.before),
                "margin-bottom" | "margin-block-end" => para.after = length(&value).or(para.after),
                "margin" => {
                    let sides: Vec<Option<model::Twips>> =
                        value.split_ascii_whitespace().map(length).collect();
                    let [top, right, bottom, left] = match sides.as_slice() {
                        [all] => [*all; 4],
                        [vertical, across] => [*vertical, *across, *vertical, *across],
                        [top, across, bottom] => [*top, *across, *bottom, *across],
                        [top, right, bottom, left] => [*top, *right, *bottom, *left],
                        _ => [None; 4],
                    };
                    para.before = top.or(para.before);
                    para.after = bottom.or(para.after);
                    para.indent = left.unwrap_or(para.indent);
                    para.indent_right = right.unwrap_or(para.indent_right);
                }
                _ => {}
            }
        }
    }
    if let Some(line) = attribute(node, "data-line-height")
        .and_then(|line| line.trim().parse::<f64>().ok())
        .filter(|line| line.is_finite() && *line > 0.0 && *line <= 100.0)
    {
        para.line = Some((line * 100.0).round() as u16);
    }
    para
}

/// A list's style: the name Docs writes, a `list-style-type` on the list or
/// on its first item (as Google Docs writes it), or the old `type`.
fn list_style_of(node: &NodeRef) -> Option<ListStyle> {
    if let Some(style) = attribute(node, "data-list").and_then(ListStyle::from_name) {
        return Some(style);
    }
    let from_css = |value: &str| {
        let value = value.trim().to_ascii_lowercase();
        let first = value.split_ascii_whitespace().next().unwrap_or_default();
        Some(match first {
            "disc" => ListStyle::Disc,
            "circle" => ListStyle::Circle,
            "square" => ListStyle::Square,
            "decimal" => ListStyle::Decimal,
            "lower-alpha" | "lower-latin" => ListStyle::LowerAlpha,
            "upper-alpha" | "upper-latin" => ListStyle::UpperAlpha,
            "lower-roman" => ListStyle::LowerRoman,
            "upper-roman" => ListStyle::UpperRoman,
            _ => return None,
        })
    };
    let first_item = node.children().find(|child| element(child) == Some("li"));
    for holder in std::iter::once(*node).chain(first_item) {
        if let Some(css) = attribute(&holder, "style") {
            for (name, value) in style::declarations(css) {
                if (name == "list-style-type" || name == "list-style")
                    && let Some(style) = from_css(&value)
                {
                    return Some(style);
                }
            }
        }
    }
    match attribute(node, "type")?.trim() {
        "1" => Some(ListStyle::Decimal),
        "a" => Some(ListStyle::LowerAlpha),
        "A" => Some(ListStyle::UpperAlpha),
        "i" => Some(ListStyle::LowerRoman),
        "I" => Some(ListStyle::UpperRoman),
        other => from_css(other),
    }
}

/// A picture: from its `data:` address, or by the hash Docs wrote beside it
/// when its contents were missing. Pictures from elsewhere on the web are
/// left out, since showing one would tell its site who's reading.
fn image(node: NodeRef, pictures: &mut Pictures) -> Option<Block> {
    let hash = match attribute(&node, "src").and_then(Picture::from_data_address) {
        Some(found) => {
            let hash = found.hash.clone();
            pictures.entry(hash.clone()).or_insert(found);
            hash
        }
        None => attribute(&node, "data-hash")
            .map(str::trim)
            .filter(|hash| picture::is_hash(hash))?
            .to_owned(),
    };
    Some(Block::Image(Image {
        hash,
        alt: attribute(&node, "alt").unwrap_or_default().to_owned(),
        width: attribute(&node, "width").and_then(|width| width.trim().parse().ok()),
    }))
}

/// The pictures inside a paragraph or heading, as blocks after it, since
/// the editor's pictures are on lines of their own.
fn images_in(node: NodeRef, out: &mut Vec<Block>, pictures: &mut Pictures) {
    for child in node.descendants() {
        if element(&child) == Some("img") {
            out.extend(google_line(child).or_else(|| image(child, pictures)));
        }
    }
}

/// Whether a paragraph holds one of Google Docs' lines.
fn has_line(node: NodeRef) -> bool {
    node.descendants().any(|child| google_line(child).is_some())
}

/// Google Docs' horizontal line, which it copies as a picture of the line
/// named "horizontal line", as thick as the picture is tall, in the color
/// of the text around it.
fn google_line(node: NodeRef) -> Option<Block> {
    if element(&node) != Some("img")
        || !attribute(&node, "title")
            .is_some_and(|title| title.trim().eq_ignore_ascii_case("horizontal line"))
    {
        return None;
    }
    let thickness = attribute(&node, "height")
        .and_then(|height| height.trim().parse::<f64>().ok())
        .filter(|height| *height > 0.0)
        .and_then(|height| model::twips(height * 0.75));
    let color = node
        .ancestors()
        .take_while(|ancestor| !matches!(element(ancestor), Some("p" | "td" | "th" | "li")))
        .find_map(|ancestor| {
            attribute(&ancestor, "style").and_then(|css| {
                style::declarations(css)
                    .into_iter()
                    .rev()
                    .find_map(|(name, value)| {
                        (name == "color").then(|| Color::from_css(&value)).flatten()
                    })
            })
        })
        .or(Some(Color::BLACK));
    Some(Block::Rule(model::Rule { thickness, color }))
}

/// A line's thickness and color, from the style Docs writes on it.
fn rule_of(node: &NodeRef) -> model::Rule {
    let mut rule = model::Rule::default();
    if let Some(css) = attribute(node, "style") {
        for (name, value) in style::declarations(css) {
            if matches!(name.as_str(), "border-top" | "border") {
                let (thickness, color) = border_of(&value);
                rule.thickness = thickness
                    .filter(|thickness| *thickness > 0)
                    .or(rule.thickness);
                rule.color = color.or(rule.color);
            }
        }
    }
    rule
}

/// A border's thickness and color from CSS such as `solid #000000 1pt`,
/// as Google Docs writes it, `1.0pt solid windowtext`, as Word does, or
/// `none`. A style with no thickness is CSS's medium, 3 pixels.
fn border_of(value: &str) -> (Option<model::Twips>, Option<Color>) {
    let mut thickness = None;
    let mut color = None;
    let mut styled = false;
    for word in value.split_ascii_whitespace() {
        let word = word.to_ascii_lowercase();
        match word.as_str() {
            "none" | "hidden" => return (Some(0), None),
            "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset" | "outset" => {
                styled = true;
            }
            "thin" => thickness = Some(0.75),
            "medium" => thickness = Some(2.25),
            "thick" => thickness = Some(3.75),
            "windowtext" => color = Some(Color::BLACK),
            _ => {
                if let Some(points) = style::length_from_css(&word) {
                    thickness = Some(points.max(0.0));
                } else if let Some(found) = Color::from_css(&word) {
                    color = Some(found);
                }
            }
        }
    }
    let thickness = match thickness {
        Some(points) => model::twips(points),
        None if styled => model::twips(2.25),
        None => None,
    };
    (thickness, color)
}

/// The inline contents of a paragraph or heading, with the text styles
/// the block itself sets, as Word's web pages often do.
fn inlines_of(node: NodeRef, notes: &NoteTexts) -> Vec<Inline> {
    let mut runs = Runs::default();
    let mut marks = Marks::default();
    apply_style(&node, &mut marks, true);
    for child in node.children() {
        read_inline(child, &marks, &mut runs, notes);
    }
    runs.finish()
}

/// Sets the font Docs uses for a font read from a style, and the weight its
/// name gives, such as "Raleway Light", unless the style sets a weight.
fn set_font(marks: &mut Marks, font: &fonts::Resolved, weighted: bool) {
    marks.font = Some(font.font.name.to_owned());
    if !weighted && let Some(weight) = font.weight {
        marks.bold = weight == 700;
        marks.weight = (weight != 700).then_some(weight);
    }
}

/// The text styles an element's `style` attribute sets, and an old-style
/// `<font>`'s. A block's background is shading, not a highlight, so only
/// inline elements highlight.
fn apply_style(node: &NodeRef, marks: &mut Marks, block: bool) {
    if element(node) == Some("font") {
        if let Some(color) = attribute(node, "color").and_then(Color::from_css) {
            marks.color = Some(color);
        }
        if let Some(face) = attribute(node, "face").and_then(style::font_from_css) {
            set_font(marks, &face, false);
        }
        if let Some(size) = attribute(node, "size").and_then(style::size_from_font_tag) {
            marks.size = Some(size);
        }
    }
    let Some(css) = attribute(node, "style") else {
        return;
    };
    let declarations = style::declarations(css);
    // Docs' own weight, which the style shows with bold added.
    let own_weight = attribute(node, "data-weight")
        .and_then(|weight| weight.trim().parse::<u16>().ok())
        .filter(|weight| weight % 100 == 0 && (100..=900).contains(weight));
    if own_weight.is_some() {
        marks.weight = own_weight;
    }
    let weighted =
        own_weight.is_some() || declarations.iter().any(|(name, _)| name == "font-weight");
    // Programs that raise text also make it smaller, which the editor
    // already does for superscript and subscript.
    let mut raised = false;
    for (name, value) in &declarations {
        if name == "vertical-align" {
            match value.trim().to_ascii_lowercase().as_str() {
                "super" => {
                    marks.script = Some(Script::Super);
                    raised = true;
                }
                "sub" => {
                    marks.script = Some(Script::Sub);
                    raised = true;
                }
                "baseline" => marks.script = None,
                _ => {}
            }
        }
    }
    for (name, value) in &declarations {
        let lower = value.trim().to_ascii_lowercase();
        match name.as_str() {
            "color" => {
                if let Some(color) = Color::from_css(value) {
                    marks.color = Some(color);
                }
            }
            "background-color" | "background" if !block => {
                if let Some(color) = Color::from_css(value) {
                    marks.highlight = Some(color);
                }
            }
            "font-size" if !raised => {
                if let Some(size) = style::size_from_css(value, marks.size) {
                    marks.size = Some(size);
                }
            }
            "font-family" => {
                if let Some(font) = style::font_from_css(value) {
                    set_font(marks, &font, weighted);
                }
            }
            // Only 700 is bold. Other weights are their own, as the editor
            // keeps them.
            "font-weight" if own_weight.is_none() => match lower.as_str() {
                "bold" | "bolder" => {
                    marks.bold = true;
                    marks.weight = None;
                }
                "normal" | "lighter" => {
                    marks.bold = false;
                    marks.weight = None;
                }
                weight => {
                    if let Ok(weight) = weight.parse::<f64>()
                        && weight.is_finite()
                    {
                        let weight = ((weight / 100.0).round() * 100.0).clamp(100.0, 900.0) as u16;
                        marks.bold = weight == 700;
                        marks.weight = (weight != 400 && weight != 700).then_some(weight);
                    }
                }
            },
            "font-style" => match lower.as_str() {
                "italic" | "oblique" => marks.italic = true,
                "normal" => marks.italic = false,
                _ => {}
            },
            "text-decoration" | "text-decoration-line" => {
                if lower.contains("underline") {
                    marks.underline = true;
                }
                if lower.contains("line-through") {
                    marks.strike = true;
                }
            }
            _ => {}
        }
    }
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

fn read_inline(node: NodeRef, marks: &Marks, runs: &mut Runs, notes: &NoteTexts) {
    let name = match node.value() {
        Node::Text(text) => {
            add_text(runs, &text[..], marks);
            return;
        }
        Node::Element(_) => element(&node).unwrap_or("other"),
        _ => return,
    };
    if SKIPPED.contains(&name) || attribute(&node, "data-footnote-backref").is_some() {
        return;
    }
    if attribute(&node, "data-footnote-ref").is_some() {
        if let Some(text) = note_of(node, notes) {
            runs.runs.push(Run::Note(text.clone()));
        }
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
        "sup" => marks.script = Some(Script::Super),
        "sub" => marks.script = Some(Script::Sub),
        "mark" => marks.highlight = Some(Color([255, 255, 0])),
        "a" => {
            if let Some(href) = attribute(&node, "href").and_then(model::safe_link) {
                marks.link = Some(href);
            }
        }
        _ => {}
    }
    if let Some(field) = attribute(&node, "data-field").and_then(Field::from_name) {
        marks.field = Some(field);
    }
    apply_style(&node, &mut marks, false);
    for child in node.children() {
        read_inline(child, &marks, runs, notes);
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

fn list(node: NodeRef, numbered: bool, pictures: &mut Pictures, notes: &NoteTexts) -> Block {
    let checklist = attribute(&node, "class")
        .is_some_and(|class| class.split_ascii_whitespace().any(|c| c == "checklist"))
        || attribute(&node, "data-type") == Some("checklist")
        || node
            .children()
            .any(|child| attribute(&child, "data-checked").is_some())
        // GitHub's task lists, with a box at the start of each item.
        || node.children().any(|item| {
            element(&item) == Some("li")
                && item.children().any(|child| {
                    element(&child) == Some("input") && attribute(&child, "type") == Some("checkbox")
                })
        });
    let mut items: Vec<(bool, Vec<Block>)> = Vec::new();
    for child in node.children() {
        match element(&child) {
            Some("li") => {
                let mut blocks = Vec::new();
                read_blocks(child, &mut blocks, pictures, notes);
                items.push((is_checked(&child), blocks));
            }
            // A list right inside a list, which some programs write, goes
            // with the item before it.
            Some("ul" | "ol") => {
                let nested = list(child, element(&child) == Some("ol"), pictures, notes);
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
        Block::Numbers(
            start,
            list_style_of(&node),
            items.into_iter().map(|(_, blocks)| blocks).collect(),
        )
    } else {
        Block::Bullets(
            list_style_of(&node),
            items.into_iter().map(|(_, blocks)| blocks).collect(),
        )
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

fn table(node: NodeRef, pictures: &mut Pictures, notes: &NoteTexts) -> Block {
    // Google Docs and Word name a table without lines `border:none` and
    // leave its cells' borders out.
    let lineless = attribute(&node, "border").is_some_and(|border| border.trim() == "0")
        || attribute(&node, "style").is_some_and(|css| {
            style::declarations(css)
                .into_iter()
                .any(|(name, value)| name == "border" && border_of(&value).0 == Some(0))
        });
    let mut columns = Vec::new();
    let mut rows = Vec::new();
    collect_rows(node, &mut rows, &mut columns, lineless, pictures, notes);
    if columns.iter().any(Option::is_some) {
        widths_from_columns(&mut rows, &columns);
    }
    Block::Table(rows)
}

/// A length in pixels from an HTML `width` or `height`, or a CSS length.
fn pixels(text: &str) -> Option<u32> {
    let text = text.trim();
    let pixels = match text.parse::<f64>() {
        Ok(pixels) => pixels,
        Err(_) => style::length_from_css(text)? / 0.75,
    };
    (1.0..100_000.0)
        .contains(&pixels)
        .then(|| pixels.round() as u32)
}

/// An element's width from its `width` or its style's, in pixels.
fn width_of(node: &NodeRef) -> Option<u32> {
    let from_style = attribute(node, "style").and_then(|css| {
        style::declarations(css)
            .into_iter()
            .rev()
            .find_map(|(name, value)| (name == "width").then(|| pixels(&value)).flatten())
    });
    from_style.or_else(|| attribute(node, "width").and_then(pixels))
}

fn collect_rows(
    node: NodeRef,
    rows: &mut Vec<Row>,
    columns: &mut Vec<Option<u32>>,
    lineless: bool,
    pictures: &mut Pictures,
    notes: &NoteTexts,
) {
    for child in node.children() {
        match element(&child) {
            Some("thead" | "tbody" | "tfoot") => {
                collect_rows(child, rows, columns, lineless, pictures, notes);
            }
            // Google Docs gives each column's width in pixels.
            Some("colgroup") => {
                let cols: Vec<NodeRef> = child
                    .children()
                    .filter(|col| element(col) == Some("col"))
                    .collect();
                if cols.is_empty() {
                    columns.push(width_of(&child));
                }
                for col in cols {
                    let span = attribute(&col, "span")
                        .and_then(|span| span.trim().parse::<usize>().ok())
                        .unwrap_or(1)
                        .clamp(1, model::MAX_SPAN as usize);
                    columns.extend(std::iter::repeat_n(width_of(&col), span));
                }
            }
            Some("col") => columns.push(width_of(&child)),
            Some("tr") => {
                let height = attribute(&child, "style")
                    .and_then(|css| {
                        style::declarations(css)
                            .into_iter()
                            .rev()
                            .find_map(|(name, value)| {
                                (name == "height")
                                    .then(|| style::length_from_css(&value))
                                    .flatten()
                            })
                    })
                    .or_else(|| {
                        attribute(&child, "height")
                            .and_then(pixels)
                            .map(|px| f64::from(px) * 0.75)
                    })
                    .and_then(model::twips)
                    .filter(|height| *height > 0);
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
                    let colspan = span("colspan");
                    let colwidth = attribute(&cell, "data-colwidth")
                        .and_then(|widths| {
                            widths
                                .split(',')
                                .map(|width| width.trim().parse().ok())
                                .collect::<Option<Vec<u32>>>()
                        })
                        // Word gives each cell its width.
                        .or_else(|| {
                            (colspan == 1)
                                .then(|| width_of(&cell))
                                .flatten()
                                .map(|width| vec![width])
                        });
                    let (borders, border_color, padding) = cell_box(&cell, lineless);
                    let mut content = Vec::new();
                    read_blocks(cell, &mut content, pictures, notes);
                    cells.push(Cell {
                        header,
                        colspan,
                        rowspan: span("rowspan"),
                        colwidth,
                        background: cell_color(cell),
                        borders,
                        border_color,
                        padding,
                        content,
                    });
                }
                rows.push(Row { height, cells });
            }
            _ => {}
        }
    }
}

/// Gives each cell without widths of its own the widths of the columns
/// it's in, from the table's `<col>`s.
fn widths_from_columns(rows: &mut [Row], columns: &[Option<u32>]) {
    // How many more rows each column is taken by a cell from a row above.
    let mut taken: Vec<u32> = Vec::new();
    for row in rows {
        let mut column = 0;
        for cell in &mut row.cells {
            while taken.get(column).is_some_and(|rows| *rows > 0) {
                column += 1;
            }
            let span = cell.colspan.clamp(1, model::MAX_SPAN) as usize;
            if taken.len() < column + span {
                taken.resize(column + span, 0);
            }
            for rows in &mut taken[column..column + span] {
                *rows = cell.rowspan.clamp(1, model::MAX_SPAN);
            }
            if cell.colwidth.is_none() {
                cell.colwidth = columns
                    .get(column..column + span)
                    .and_then(|widths| widths.iter().copied().collect::<Option<Vec<u32>>>());
            }
            column += span;
        }
        for rows in &mut taken {
            *rows = rows.saturating_sub(1);
        }
    }
}

/// A cell's borders, their color, and its padding, from its style. A cell
/// that names any border has none on the sides it leaves out, as CSS
/// draws it; one that names none has the usual lines, or none in a table
/// without lines.
type CellBox = (
    [Option<model::Twips>; 4],
    Option<Color>,
    Option<[model::Twips; 4]>,
);

fn cell_box(cell: &NodeRef, lineless: bool) -> CellBox {
    let mut borders: [Option<model::Twips>; 4] = [None; 4];
    let mut colors: [Option<Color>; 4] = [None; 4];
    let mut padding: [Option<model::Twips>; 4] = [None; 4];
    let mut named = false;
    let side = |name: &str| SIDES.iter().position(|side| *side == name);
    let four = |value: &str| -> Vec<String> {
        let parts: Vec<String> = value.split_ascii_whitespace().map(str::to_owned).collect();
        match parts.as_slice() {
            [all] => vec![all.clone(); 4],
            [vertical, across] => vec![
                vertical.clone(),
                across.clone(),
                vertical.clone(),
                across.clone(),
            ],
            [top, across, bottom] => {
                vec![top.clone(), across.clone(), bottom.clone(), across.clone()]
            }
            [_, _, _, _] => parts,
            _ => Vec::new(),
        }
    };
    let length = |text: &str| style::length_from_css(text).and_then(model::twips);
    for (name, value) in attribute(cell, "style")
        .map(style::declarations)
        .unwrap_or_default()
    {
        let name = name.as_str();
        if name == "border" {
            named = true;
            let (thickness, color) = border_of(&value);
            borders = [thickness.or(Some(0)); 4];
            colors = [color; 4];
        } else if let Some(at) = name.strip_prefix("border-").and_then(side) {
            named = true;
            let (thickness, color) = border_of(&value);
            borders[at] = thickness.or(Some(0));
            colors[at] = color;
        } else if name == "border-width" {
            named = true;
            for (at, part) in four(&value).iter().enumerate() {
                borders[at] = border_of(part).0.or(borders[at]);
            }
        } else if name == "border-style" {
            named = true;
            for (at, part) in four(&value).iter().enumerate() {
                if matches!(part.to_ascii_lowercase().as_str(), "none" | "hidden") {
                    borders[at] = Some(0);
                } else if borders[at].is_none() {
                    borders[at] = model::twips(2.25);
                }
            }
        } else if name == "border-color" {
            for (at, part) in four(&value).iter().enumerate() {
                colors[at] = border_of(part).1.or(colors[at]);
            }
        } else if let Some((at, part)) = name
            .strip_prefix("border-")
            .and_then(|rest| rest.split_once('-'))
            .and_then(|(at, part)| Some((side(at)?, part)))
            .filter(|(_, part)| matches!(*part, "width" | "style" | "color"))
        {
            named = true;
            match part {
                "width" => borders[at] = border_of(&value).0.or(borders[at]),
                "style"
                    if matches!(
                        value.trim().to_ascii_lowercase().as_str(),
                        "none" | "hidden"
                    ) =>
                {
                    borders[at] = Some(0);
                }
                "style" if borders[at].is_none() => borders[at] = model::twips(2.25),
                "color" => colors[at] = border_of(&value).1.or(colors[at]),
                _ => {}
            }
        } else if name == "padding" {
            for (at, part) in four(&value).iter().enumerate() {
                padding[at] = length(part).or(padding[at]);
            }
        } else if let Some(at) = name.strip_prefix("padding-").and_then(side) {
            padding[at] = length(&value).or(padding[at]);
        }
    }
    let borders = if named {
        borders.map(|border| Some(border.unwrap_or(0)))
    } else if lineless {
        [Some(0); 4]
    } else {
        [None; 4]
    };
    // One color for the cell: the first a visible border has.
    let color = (0..4)
        .find(|at| borders[*at].is_some_and(|border| border > 0) && colors[*at].is_some())
        .and_then(|at| colors[at])
        .filter(|color| *color != Color::BLACK);
    let padding = padding
        .iter()
        .any(Option::is_some)
        .then(|| padding.map(|side| side.unwrap_or(0).max(0)));
    (borders, color, padding)
}

/// A cell's color from its style, as Google Docs and Word write it, or from
/// the old `bgcolor`. White is the page's own color, which Word names on
/// every cell, so it's no color.
fn cell_color(cell: NodeRef) -> Option<Color> {
    let from_style = attribute(&cell, "style").and_then(|style| {
        style::declarations(style)
            .into_iter()
            .rev()
            .find_map(|(name, value)| match name.as_str() {
                "background-color" => Color::from_css(&value),
                // The shorthand, such as `background:yellow`, or with more in
                // it, such as `background:#fce5cd none`.
                "background" => value.split_whitespace().find_map(Color::from_css),
                _ => None,
            })
    });
    from_style
        .or_else(|| attribute(&cell, "bgcolor").and_then(Color::from_css))
        .filter(|color| *color != Color([255, 255, 255]))
}

#[cfg(test)]
pub(crate) mod tests {
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
            Block::Heading(1, Para::default(), vec![text("Trip <plan> & more")]),
            Block::Paragraph(
                Para::default(),
                vec![
                    text("Two  spaces,\u{a0}a no-break space, "),
                    marked("bold", bold.clone()),
                    marked(" and linked bold", link_bold),
                    marked(" link", link),
                    Inline::Break,
                    text(" after a break "),
                    marked(
                        "red Lobster",
                        Marks {
                            color: Some(Color([255, 0, 0])),
                            size: Some(29),
                            font: Some("Lobster".into()),
                            ..Marks::default()
                        },
                    ),
                    marked(
                        "2",
                        Marks {
                            color: Some(Color([255, 0, 0])),
                            script: Some(Script::Super),
                            highlight: Some(Color([255, 255, 0])),
                            ..Marks::default()
                        },
                    ),
                    marked(
                        "n",
                        Marks {
                            script: Some(Script::Sub),
                            ..Marks::default()
                        },
                    ),
                ],
            ),
            Block::Paragraph(Para::default(), Vec::new()),
            Block::Quote(vec![Block::Paragraph(
                Para::default(),
                vec![text("Quoted")],
            )]),
            Block::Code("fn main() {\n    println!(\"<hi>\");\n}\n".into()),
            Block::Rule(crate::model::Rule::default()),
            Block::Bullets(
                None,
                vec![
                    vec![Block::Paragraph(Para::default(), vec![text("One")])],
                    vec![
                        Block::Paragraph(Para::default(), vec![text("Two")]),
                        Block::Numbers(
                            3,
                            None,
                            vec![vec![Block::Paragraph(Para::default(), vec![text("Inner")])]],
                        ),
                    ],
                ],
            ),
            Block::Checks(vec![
                (
                    true,
                    vec![Block::Paragraph(Para::default(), vec![text("Milk")])],
                ),
                (
                    false,
                    vec![Block::Paragraph(Para::default(), vec![text("Bread")])],
                ),
            ]),
            crate::model::table(vec![
                vec![Cell {
                    header: true,
                    colspan: 2,
                    rowspan: 1,
                    colwidth: Some(vec![120, 80]),
                    background: None,
                    content: vec![Block::Paragraph(Para::default(), vec![text("Head")])],
                    ..Cell::default()
                }],
                vec![
                    Cell {
                        header: false,
                        colspan: 1,
                        rowspan: 1,
                        colwidth: None,
                        background: Some(Color([252, 229, 205])),
                        content: vec![Block::Paragraph(Para::default(), vec![text("A")])],
                        ..Cell::default()
                    },
                    Cell {
                        header: false,
                        colspan: 1,
                        rowspan: 1,
                        colwidth: None,
                        background: None,
                        content: vec![Block::Paragraph(Para::default(), Vec::new())],
                        ..Cell::default()
                    },
                ],
            ]),
        ]
    }

    /// Blocks with every part of a paragraph's layout, list styles, and a
    /// page break.
    /// A paragraph with two footnotes, one with marks and a line break.
    pub(crate) fn noted() -> Vec<Block> {
        let bold = Marks {
            bold: true,
            ..Marks::default()
        };
        vec![Block::Paragraph(
            Para::default(),
            vec![
                text("Claim"),
                Inline::Footnote(vec![
                    text("See "),
                    marked("page 4", bold),
                    Inline::Break,
                    text("Second line"),
                ]),
                text(" and more"),
                Inline::Footnote(vec![text("Two")]),
            ],
        )]
    }

    #[test]
    fn footnotes_survive_the_file() {
        let file = page(&noted(), &Parts::default(), None, None, &Pictures::new());
        assert!(
            file.contains(
                "Claim<sup><a href=\"#fn-1\" id=\"fnref-1\" data-footnote-ref>1</a></sup> and more"
            ),
            "{file}"
        );
        assert!(
            file.contains("<li id=\"fn-2\"><p>Two</p></li>\n</ol>\n</section>\n</main>"),
            "{file}"
        );
        assert_eq!(read(file.as_bytes()), noted());
    }

    #[test]
    fn reads_footnotes_as_github_writes_them() {
        let page = "<p>Claim<sup><a href=\"#user-content-fn-x\" id=\"user-content-fnref-x\" \
            data-footnote-ref>1</a></sup> and a <sup>2</sup></p>\
            <section data-footnotes class=\"footnotes\"><h2 id=\"footnote-label\">Footnotes</h2><ol>\
            <li id=\"user-content-fn-x\"><p>First <em>note</em> \
            <a href=\"#user-content-fnref-x\" data-footnote-backref>↩</a></p><p>More</p></li></ol></section>";
        let italic = Marks {
            italic: true,
            ..Marks::default()
        };
        assert_eq!(
            read(page.as_bytes()),
            vec![Block::Paragraph(
                Para::default(),
                vec![
                    text("Claim"),
                    Inline::Footnote(vec![
                        text("First "),
                        marked("note", italic),
                        Inline::Break,
                        text("More"),
                    ]),
                    text(" and a "),
                    marked(
                        "2",
                        Marks {
                            script: Some(Script::Super),
                            ..Marks::default()
                        }
                    ),
                ],
            )]
        );
    }

    /// A table of contents, and headings it lists: one with a footnote,
    /// two with the same text, an empty one it leaves out, and one in a
    /// quote.
    pub(crate) fn with_contents() -> Vec<Block> {
        vec![
            Block::Contents(model::ContentsStyle::Dotted),
            Block::Heading(
                1,
                Para::default(),
                vec![text("Plan"), Inline::Footnote(vec![text("Note")])],
            ),
            Block::Heading(2, Para::default(), vec![text("Plan")]),
            Block::Heading(2, Para::default(), Vec::new()),
            Block::Quote(vec![Block::Heading(
                3,
                Para::default(),
                vec![text("Café & tea!")],
            )]),
        ]
    }

    #[test]
    fn contents_link_to_headings() {
        let file = page(
            &with_contents(),
            &Parts::default(),
            None,
            None,
            &Pictures::new(),
        );
        assert!(
            file.contains(
                "<nav class=\"contents\" data-contents=\"dotted\">\n\
                 <p class=\"contents-1\"><a href=\"#plan\">Plan</a></p>\n\
                 <p class=\"contents-2\"><a href=\"#plan-1\">Plan</a></p>\n\
                 <p class=\"contents-3\"><a href=\"#café--tea\">Café &amp; tea!</a></p>\n</nav>"
            ),
            "{file}"
        );
        assert!(file.contains("<h1 id=\"plan\">Plan<sup>"), "{file}");
        assert!(
            file.contains("<h2 id=\"plan-1\">Plan</h2>\n<h2></h2>"),
            "{file}"
        );
        assert!(file.contains("<h3 id=\"café--tea\">"), "{file}");
        // The entries come from the headings, so reading leaves only the
        // table of contents itself.
        assert_eq!(read(file.as_bytes()), with_contents());
        // Without one, headings have no IDs.
        let plain = page(
            &with_contents()[1..],
            &Parts::default(),
            None,
            None,
            &Pictures::new(),
        );
        assert!(plain.contains("<h1>Plan<sup>"), "{plain}");
    }

    pub(crate) fn laid_out() -> Vec<Block> {
        vec![
            Block::Paragraph(
                Para {
                    kind: Some(Kind::Title),
                    align: Align::Center,
                    ..Para::default()
                },
                vec![text("Trip")],
            ),
            Block::Paragraph(
                Para {
                    kind: Some(Kind::Subtitle),
                    ..Para::default()
                },
                vec![text("Summer")],
            ),
            Block::Heading(
                5,
                Para {
                    align: Align::Right,
                    before: Some(0),
                    ..Para::default()
                },
                vec![text("Days")],
            ),
            Block::Paragraph(
                Para {
                    align: Align::Justify,
                    indent: 720,
                    indent_right: 360,
                    first_line: -360,
                    line: Some(150),
                    before: Some(240),
                    after: Some(0),
                    kind: None,
                },
                vec![text("Hanging")],
            ),
            Block::PageBreak,
            Block::Numbers(
                4,
                Some(ListStyle::UpperRoman),
                vec![vec![
                    Block::Paragraph(Para::default(), vec![text("Fourth")]),
                    Block::Bullets(
                        Some(ListStyle::Arrow),
                        vec![vec![Block::Paragraph(Para::default(), vec![text("Inner")])]],
                    ),
                ]],
            ),
            Block::Numbers(
                1,
                Some(ListStyle::DecimalParen),
                vec![vec![Block::Paragraph(Para::default(), vec![text("One")])]],
            ),
        ]
    }

    /// A table with a colored heading cell, a plain one, and a colored
    /// cell two columns wide.
    pub(crate) fn colored_cells() -> Vec<Block> {
        let cell = |header, colspan, background, words: &str| Cell {
            header,
            colspan,
            rowspan: 1,
            colwidth: None,
            background,
            content: vec![Block::Paragraph(Para::default(), vec![text(words)])],
            ..Cell::default()
        };
        vec![crate::model::table(vec![
            vec![
                cell(true, 1, Some(Color([252, 229, 205])), "Head"),
                cell(true, 1, None, "Plain"),
            ],
            vec![cell(false, 2, Some(Color([207, 226, 243])), "Wide")],
        ])]
    }

    /// A table laid out as Google Docs' resume templates are, as Google
    /// Docs copies it: no lines, columns of their own widths, rows of a
    /// least height, padding, a red top border, and a line in a cell.
    pub(crate) fn google_layout() -> Vec<Block> {
        let html = r#"<meta charset="utf-8"><div dir="ltr" style="margin-left:0pt;" align="left"><table style="border:none;border-collapse:collapse;"><colgroup><col width="235" /><col width="437" /></colgroup><tbody><tr style="height:120pt"><td style="vertical-align:top;padding:3.6pt 3.6pt 3.6pt 3.6pt;overflow:hidden;overflow-wrap:break-word;"><h2 dir="ltr" style="line-height:1.38;margin-top:6pt;margin-bottom:0pt;"><span style="font-size:11pt;color:#000000;font-weight:700;">Company / </span><span style="font-size:11pt;color:#000000;font-weight:400;">Job</span></h2></td><td style="vertical-align:top;padding:5pt 5pt 5pt 5pt;border-top:solid #ff0000 1pt;overflow:hidden;"><p dir="ltr" style="line-height:1.2;margin-top:0pt;margin-bottom:0pt;"><span style="font-size:10pt;color:#000000;font-weight:400;"><img title="horizontal line" src="data:image/png;base64,iVBORw0KGgo=" width="418" height="3" style="border:none;" /></span></p><p dir="ltr" style="line-height:1.2;margin-top:0pt;margin-bottom:0pt;"><span style="font-size:10pt;">Street</span></p></td></tr><tr style="height:0pt"><td><p>a</p></td><td><p>b</p></td></tr></tbody></table></div><br />"#;
        read(html.as_bytes())
    }

    #[test]
    fn reads_google_docs_layout_tables() {
        let blocks = google_layout();
        let Block::Table(rows) = &blocks[0] else {
            panic!("not a table: {blocks:?}");
        };
        assert_eq!(rows[0].height, Some(2400));
        assert_eq!(rows[1].height, None);
        let [first, second] = &rows[0].cells[..] else {
            panic!("not two cells");
        };
        assert_eq!(first.borders, [Some(0); 4]);
        assert_eq!(first.padding, Some([72; 4]));
        assert_eq!(first.colwidth, Some(vec![235]));
        // "Job" isn't bold, and a heading isn't bold on its own.
        let Block::Heading(2, _, runs) = &first.content[0] else {
            panic!("not a heading");
        };
        assert!(
            matches!(&runs[..], [Inline::Text(_, bold), Inline::Text(_, plain)] if bold.bold && !plain.bold)
        );
        assert_eq!(second.borders, [Some(20), Some(0), Some(0), Some(0)]);
        assert_eq!(second.border_color, Some(Color([255, 0, 0])));
        assert_eq!(second.padding, Some([100; 4]));
        assert_eq!(second.colwidth, Some(vec![437]));
        // The line is a line, without the paragraph it was in.
        assert_eq!(
            second.content[0],
            Block::Rule(model::Rule {
                thickness: Some(45),
                color: Some(Color::BLACK),
            })
        );
        assert!(matches!(&second.content[1], Block::Paragraph(_, runs) if !runs.is_empty()));
        // Cells in a table without lines have none, and the columns' widths.
        assert_eq!(rows[1].cells[1].borders, [Some(0); 4]);
        assert_eq!(rows[1].cells[1].colwidth, Some(vec![437]));
        // Docs' own file keeps all of it, and stays light, since the
        // border has a color.
        let page = page(
            &blocks,
            &model::Parts::default(),
            None,
            None,
            &Pictures::new(),
        );
        assert_eq!(read(page.as_bytes()), blocks);
        assert!(!page.contains("prefers-color-scheme"));
    }

    #[test]
    fn reads_borders_word_writes() {
        let html = r#"<table class=MsoTableGrid border=1 cellspacing=0 cellpadding=0 style='border-collapse:collapse;border:none'>
            <tr style='height:20.0pt'><td width=208 valign=top style='width:155.8pt;border:solid windowtext 1.0pt;border-left:none;padding:0in 5.4pt 0in 5.4pt'><p>One</p></td>
            <td style='border:none;border-bottom:dashed #2F5496 1.5pt'><p>Two</p></td></tr></table>"#;
        let [Block::Table(rows)] = &read(html.as_bytes())[..] else {
            panic!("not one table");
        };
        assert_eq!(rows[0].height, Some(400));
        let [one, two] = &rows[0].cells[..] else {
            panic!("not two cells");
        };
        assert_eq!(one.borders, [Some(20), Some(20), Some(20), Some(0)]);
        assert_eq!(one.border_color, None);
        assert_eq!(one.padding, Some([0, 108, 0, 108]));
        assert_eq!(one.colwidth, Some(vec![208]));
        assert_eq!(two.borders, [Some(0), Some(0), Some(30), Some(0)]);
        assert_eq!(two.border_color, Some(Color([0x2f, 0x54, 0x96])));
        // A table with no styles has the usual lines.
        let [Block::Table(rows)] = &read(b"<table><tr><td>a</td></tr></table>")[..] else {
            panic!("not one table");
        };
        assert_eq!(rows[0].cells[0].borders, [None; 4]);
    }

    #[test]
    fn keeps_cell_colors_and_leaves_out_dark_mode() {
        let blocks = colored_cells();
        let page = page(
            &blocks,
            &crate::model::Parts::default(),
            None,
            None,
            &Pictures::new(),
        );
        assert_eq!(read(page.as_bytes()), blocks);
        assert!(page.contains("<th style=\"background-color:#fce5cd\">"));
        assert!(page.contains("<td colspan=\"2\" style=\"background-color:#cfe2f3\">"));
        // Light text on a pale cell couldn't be read.
        assert!(!page.contains("prefers-color-scheme:dark"));
    }

    #[test]
    fn reads_cell_colors_other_programs_write() {
        let html = r##"<table><tr>
            <td style="border-left:solid #000 1pt;background-color:#fce5cd;padding:5pt">Google</td>
            <td style='background:yellow;padding:0in 5.4pt'>Word</td>
            <td bgcolor="#ff0000">Old</td>
            <td style="background:white">White</td>
            <td style="background:#cfe2f3 none">Shorthand</td>
            </tr></table>"##;
        let [Block::Table(rows)] = &read(html.as_bytes())[..] else {
            panic!("not one table");
        };
        let colors: Vec<_> = rows[0].cells.iter().map(|cell| cell.background).collect();
        assert_eq!(
            colors,
            [
                Some(Color([252, 229, 205])),
                Some(Color([255, 255, 0])),
                Some(Color([255, 0, 0])),
                None,
                Some(Color([207, 226, 243])),
            ]
        );
    }

    #[test]
    fn keeps_paragraph_layout_lists_and_page_breaks() {
        let blocks = laid_out();
        let page = page(
            &blocks,
            &crate::model::Parts::default(),
            None,
            None,
            &Pictures::new(),
        );
        assert_eq!(read(page.as_bytes()), blocks);
        assert!(page.contains(
            "<p style=\"text-align:justify;margin-left:36pt;margin-right:18pt;text-indent:-18pt;\
             line-height:1.72;margin-top:12pt;margin-bottom:0pt\" data-line-height=\"1.5\">"
        ));
        assert!(page.contains("<p class=\"title\" style=\"text-align:center\">Trip</p>"));
        assert!(page.contains("<ol start=\"4\" type=\"I\" data-list=\"upper-roman\">"));
        assert!(page.contains("<div class=\"page-break\"></div>"));
    }

    #[test]
    fn reads_layout_google_docs_and_word_write() {
        // As Google Docs copies it: line spacing 1.15 as 1.38, a page break
        // as a hidden rule, and a list's style on its items.
        let html = r#"<b style="font-weight:normal;" id="docs-internal-guid-1">
            <p dir="ltr" style="line-height:1.38;text-align:center;margin-top:0pt;margin-bottom:0pt;">
            <span>Centered</span></p>
            <hr style="page-break-before:always;display:none;">
            <ul style="margin-top:0;margin-bottom:0;padding-inline-start:48px;">
            <li dir="ltr" style="list-style-type:square;"><p dir="ltr"><span>Item</span></p></li></ul>
            <p class="MsoTitle" align="right" style="margin:0in 0in 8pt .5in;text-indent:-.25in;line-height:107%">Word</p>
            </b>"#;
        let blocks = read(html.as_bytes());
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph(
                    Para {
                        align: Align::Center,
                        line: Some(115),
                        before: Some(0),
                        after: Some(0),
                        ..Para::default()
                    },
                    vec![text("Centered")]
                ),
                Block::PageBreak,
                Block::Bullets(
                    Some(ListStyle::Square),
                    vec![vec![Block::Paragraph(Para::default(), vec![text("Item")])]]
                ),
                Block::Paragraph(
                    Para {
                        kind: Some(Kind::Title),
                        align: Align::Right,
                        indent: 720,
                        first_line: -360,
                        line: Some(107),
                        before: Some(0),
                        after: Some(160),
                        indent_right: 0,
                    },
                    vec![text("Word")]
                ),
            ]
        );
    }

    #[test]
    fn reads_back_what_it_wrote() {
        let blocks = sample();
        let page = page(
            &blocks,
            &crate::model::Parts::default(),
            None,
            Some(b"state"),
            &Pictures::new(),
        );
        assert!(is_document(page.as_bytes()));
        assert_eq!(read(page.as_bytes()), blocks);
        assert_eq!(state_in(page.as_bytes()), Some(b"state".to_vec()));
        assert!(page.contains("<title>Trip &lt;plan&gt; &amp; more</title>"));
    }

    /// A header with a page number in it, a footer, and a first page with
    /// only a footer of its own.
    pub(crate) fn parts() -> Parts {
        let small = Marks {
            size: style::half_points(9.0),
            ..Marks::default()
        };
        let number = Marks {
            field: Some(Field::PageNumber),
            ..small.clone()
        };
        let count = Marks {
            field: Some(Field::PageCount),
            ..small.clone()
        };
        Parts {
            header: vec![Block::Paragraph(
                Para::default(),
                vec![
                    marked("Page ", small.clone()),
                    marked("#", number),
                    marked(" of ", small),
                    marked("#", count),
                ],
            )],
            footer: vec![Block::Paragraph(Para::default(), vec![text("Trip plan")])],
            first_header: Vec::new(),
            first_footer: vec![Block::Paragraph(Para::default(), vec![text("Draft")])],
            different_first: true,
        }
    }

    #[test]
    fn keeps_headers_footers_and_page_numbers() {
        let blocks = sample();
        let written = page(&blocks, &parts(), None, None, &Pictures::new());
        assert!(
            written.contains("<main data-first-page=\"different\">"),
            "{written}"
        );
        assert!(
            written.contains("<header data-part=\"header\">"),
            "{written}"
        );
        assert!(
            written.contains("<span data-field=\"page\">#</span>"),
            "{written}"
        );
        assert!(
            written.contains("<footer data-part=\"first-footer\">"),
            "{written}"
        );
        let read = read_document(written.as_bytes());
        assert_eq!(read.blocks, blocks);
        assert_eq!(read.parts, parts());
        // Parts with nothing in them are none, and the first page's own go
        // when it no longer differs.
        let mut cleared = parts();
        cleared.header = vec![Block::Paragraph(Para::default(), Vec::new())];
        cleared.different_first = false;
        let cleared = cleared.normalized();
        assert!(cleared.header.is_empty() && cleared.first_footer.is_empty());
        let written = page(&blocks, &cleared, None, None, &Pictures::new());
        assert!(
            !written.contains("<header data-part") && written.contains("<main>"),
            "{written}"
        );
    }

    #[test]
    fn keeps_the_page_setup() {
        let blocks = sample();
        let setup = PageSetup {
            size: PageSize::A4,
            landscape: true,
            margins: [36.0, 72.0, 56.69, 144.0],
        };
        let written = page(
            &blocks,
            &crate::model::Parts::default(),
            Some(&setup),
            None,
            &Pictures::new(),
        );
        assert!(written.contains("@page{size:297mm 210mm;margin:36pt 72pt 56.69pt 144pt}"));
        let read = read_document(written.as_bytes());
        assert_eq!((read.blocks, read.setup), (blocks.clone(), Some(setup)));
        // Without one, there's none to read.
        let plain = page(
            &blocks,
            &crate::model::Parts::default(),
            None,
            None,
            &Pictures::new(),
        );
        assert!(!plain.contains("@page"));
        assert_eq!(read_document(plain.as_bytes()).setup, None);
        // A size Docs doesn't know is no setup, and odd margins are fixed.
        assert_eq!(
            read_document(b"<main data-page=\"b5\"><p>x</p></main>").setup,
            None
        );
        assert_eq!(
            read_document(b"<main data-page=\"letter\" data-margins=\"1 2\"><p>x</p></main>").setup,
            Some(PageSetup::new(PageSize::Letter))
        );
    }

    #[test]
    fn reads_pictures_other_programs_write() {
        let picture = Picture::read(crate::picture::tests::png(4, 3)).unwrap();
        let address = picture.data_address();
        let hash = picture::hash(b"elsewhere");
        let page = format!(
            "<p>Before<span><img src=\"{address}\" alt=\"Dot\" width=\"200\"></span></p>\
             <img src=\"https://tracker.example/pixel.png\" alt=\"Remote\">\
             <img alt=\"Kept\" data-hash=\"{hash}\">\
             <img src=\"data:image/svg+xml;base64,PHN2Zy8+\" alt=\"Drawing\">"
        );
        let read = read_document(page.as_bytes());
        assert_eq!(
            read.blocks,
            vec![
                Block::Paragraph(
                    Para::default(),
                    vec![Inline::Text("Before".into(), Marks::default())]
                ),
                Block::Image(Image {
                    hash: picture.hash.clone(),
                    alt: "Dot".into(),
                    width: Some(200),
                }),
                // Its contents are missing, but it keeps its place.
                Block::Image(Image {
                    hash,
                    alt: "Kept".into(),
                    width: None,
                }),
            ]
        );
        assert_eq!(read.pictures.len(), 1);
        assert_eq!(read.pictures[&picture.hash], picture);
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
            background: None,
            content: vec![Block::Paragraph(Para::default(), vec![text(text_)])],
            ..Cell::default()
        };
        assert_eq!(
            read(html.as_bytes()),
            vec![
                // Its color stays, and its script goes.
                Block::Heading(
                    2,
                    Para::default(),
                    vec![marked(
                        "Plan",
                        Marks {
                            color: Some(Color([255, 0, 0])),
                            ..Marks::default()
                        }
                    )]
                ),
                Block::Paragraph(
                    Para::default(),
                    vec![
                        text("Some "),
                        marked("bold", bold),
                        text(" and "),
                        marked("italic", italic),
                        text(" text, a bad link."),
                    ]
                ),
                Block::Paragraph(
                    Para::default(),
                    vec![text("Loose text"), Inline::Break, text("after a break")]
                ),
                Block::Bullets(
                    None,
                    vec![
                        vec![
                            Block::Paragraph(Para::default(), vec![text("One")]),
                            Block::Bullets(
                                None,
                                vec![vec![Block::Paragraph(
                                    Para::default(),
                                    vec![text("Nested")]
                                )]]
                            ),
                        ],
                        vec![Block::Paragraph(Para::default(), vec![text("Two")])],
                    ]
                ),
                crate::model::table(vec![vec![cell(true, "H")], vec![cell(false, "C")]]),
            ]
        );
    }

    #[test]
    fn keeps_fonts_and_weights() {
        let marks = |font: Option<&str>, weight: Option<u16>, bold: bool| Marks {
            font: font.map(str::to_owned),
            weight,
            bold,
            ..Marks::default()
        };
        let blocks = vec![Block::Paragraph(
            Para::default(),
            vec![
                marked("light ", marks(Some("Raleway"), Some(300), false)),
                marked("light and bold ", marks(Some("Raleway"), Some(300), true)),
                marked("black ", marks(Some("Raleway"), Some(900), false)),
                marked("Carlito", marks(Some("Carlito"), None, false)),
            ],
        )];
        let written = page(&blocks, &Parts::default(), None, None, &Pictures::new());
        // Bold shows a light weight as regular, and Carlito names Calibri
        // after it for browsers without it.
        assert!(
            written.contains("<strong><span data-weight=\"300\" style=\"font-weight:400\">light and bold </span></strong>"),
            "{written}"
        );
        assert!(
            written.contains("font-family:'Carlito','Calibri'"),
            "{written}"
        );
        assert_eq!(read(written.as_bytes()), blocks);

        // Other programs' weights, and fonts Docs has in place of theirs.
        // Only 700 is bold, and a weight inside bold is its own.
        let other = r#"<p><span style="font-family:'Raleway Light'">a</span><span style="font-family:Raleway;font-weight:800">b</span><span style="font-family:Verdana;font-weight:600">c</span><span style="font-family:'Comic Sans MS';font-weight:bold">d</span><b><span style="font-weight:300">e</span></b></p>"#;
        assert_eq!(
            read(other.as_bytes()),
            vec![Block::Paragraph(
                Para::default(),
                vec![
                    marked("a", marks(Some("Raleway"), Some(300), false)),
                    marked("b", marks(Some("Raleway"), Some(800), false)),
                    marked("c", marks(Some("Noto Sans"), Some(600), false)),
                    marked("d", marks(Some("Comic Relief"), None, true)),
                    // Arimo has no light, so it's regular.
                    marked("e", marks(None, None, false)),
                ],
            )]
        );
    }

    #[test]
    fn reads_the_styles_google_docs_and_word_write() {
        // As Google Docs puts a paste on the clipboard.
        let google = r#"<meta charset="utf-8"><b style="font-weight:normal;" id="docs-internal-guid-1"><p dir="ltr"><span style="font-size:11pt;font-family:Arial,sans-serif;color:#000000;background-color:transparent;font-weight:400;">Plain </span><span style="font-size:11pt;font-family:Arial,sans-serif;color:#ff0000;background-color:#ffff00;font-weight:700;">loud</span><span style="font-size:6.6pt;font-family:Arial,sans-serif;color:#000000;vertical-align:super;">2</span></p></b>"#;
        // Arial is Arimo, the font text has anyway.
        let arial = Marks {
            size: Some(22),
            ..Marks::default()
        };
        assert_eq!(
            read(google.as_bytes()),
            vec![Block::Paragraph(
                Para::default(),
                vec![
                    marked("Plain ", arial.clone()),
                    marked(
                        "loud",
                        Marks {
                            bold: true,
                            color: Some(Color([255, 0, 0])),
                            highlight: Some(Color([255, 255, 0])),
                            ..arial.clone()
                        }
                    ),
                    marked(
                        "2",
                        Marks {
                            script: Some(Script::Super),
                            size: None,
                            ..arial
                        }
                    ),
                ]
            )]
        );
        // As Word saves a web page, with styles on the paragraph, an old
        // <font>, and a font that isn't a name, which is dropped.
        let word = r#"<p class=MsoNormal style='font-family:"Times New Roman";font-size:12.0pt'>A <font color="navy" size="5">big</font> <span style="font-family:url(x);color:#ff0000ff">word</span></p>"#;
        let times = Marks {
            size: Some(24),
            font: Some("Tinos".into()),
            ..Marks::default()
        };
        assert_eq!(
            read(word.as_bytes()),
            vec![Block::Paragraph(
                Para::default(),
                vec![
                    marked("A ", times.clone()),
                    marked(
                        "big",
                        Marks {
                            color: Some(Color([0, 0, 128])),
                            size: Some(36),
                            ..times.clone()
                        }
                    ),
                    marked(" ", times.clone()),
                    marked(
                        "word",
                        Marks {
                            color: Some(Color([255, 0, 0])),
                            ..times
                        }
                    ),
                ]
            )]
        );
    }

    #[test]
    fn colored_documents_stay_light() {
        let plain = page(
            &[Block::Paragraph(Para::default(), vec![text("a")])],
            &crate::model::Parts::default(),
            None,
            None,
            &Pictures::new(),
        );
        assert!(plain.contains("prefers-color-scheme:dark"));
        let red = Marks {
            color: Some(Color([255, 0, 0])),
            ..Marks::default()
        };
        let colored = page(
            &[Block::Paragraph(Para::default(), vec![marked("a", red)])],
            &crate::model::Parts::default(),
            None,
            None,
            &Pictures::new(),
        );
        assert!(!colored.contains("prefers-color-scheme:dark"));
        assert!(colored.contains("<span style=\"color:#ff0000\">a</span>"));
    }

    #[test]
    fn an_empty_page_is_one_empty_paragraph() {
        assert_eq!(
            read(b""),
            vec![Block::Paragraph(Para::default(), Vec::new())]
        );
        assert_eq!(
            read(b"<p>   \n  </p>"),
            vec![Block::Paragraph(Para::default(), Vec::new())]
        );
    }
}
