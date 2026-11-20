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

use std::fmt::Write as _;

use scraper::{Html, Node};

use crate::model::{
    self, Align, Block, Cell, Image, Inline, Kind, ListStyle, Marks, PageSetup, PageSize, Para,
    Script,
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
/// that aren't part of the text. Text is Arial at 11 points, as in Google
/// Docs and the Word download, or the fonts made to match it. Lines and
/// paragraphs are spaced as the Word download spaces them: 1.15 lines, 6
/// points after each paragraph, and Google Docs' space around headings.
const STYLE: &str = "\
body{margin:0;background:#f8f9fa;color:#1f1f1f;font:11pt/1.3225 Arial,Arimo,'Liberation Sans',sans-serif}\
main{box-sizing:border-box;max-width:816px;margin:24px auto;padding:72px 96px;background:#fff;\
box-shadow:0 1px 3px rgba(0,0,0,.15)}\
p,h1,h2,h3,h4,h5,h6{white-space:pre-wrap}\
p{margin:0 0 6pt}\
h1,h2,h3,h4,h5,h6{font-weight:700}\
h1{font-size:20pt;margin:20pt 0 6pt}h2{font-size:16pt;margin:18pt 0 6pt}h3{font-size:14pt;margin:16pt 0 4pt}\
h4{font-size:12pt;margin:14pt 0 4pt}h5{font-size:11pt;margin:12pt 0 4pt}h6{font-size:11pt;font-style:italic;margin:12pt 0 4pt}\
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
table{border-collapse:collapse;margin:0 0 .6em}\
td,th{border:1px solid #c4c7c5;padding:4px 8px;vertical-align:top;min-width:40px}\
th{background:#f1f3f4;text-align:left}\
td>p:last-child,th>p:last-child,li>p:last-child{margin:0}\
ul.checklist{list-style:none;padding-left:4px}\
ul.checklist>li>input{float:left;margin:6px 8px 0 0}\
ul.checklist>li[data-checked=true]>p{text-decoration:line-through;color:#5f6368}\
@media (max-width:860px){main{margin:0;padding:24px 16px;box-shadow:none}}\
@media print{body{background:none}main{max-width:none;margin:0;padding:0;box-shadow:none}}";

/// Dark colors, on screens only. A document with colored text or
/// highlights stays as it was written, dark on white, so its colors still
/// read.
const DARK_STYLE: &str = "@media screen and (prefers-color-scheme:dark){body{background:#131314;color:#e3e3e3}\
main{background:#1f1f1f}pre,th{background:#2d2f31}td,th{border-color:#444746}p.subtitle{color:#9aa0a6}\
blockquote{border-color:#444746;color:#c4c7c5}}";

/// The whole file for `blocks` on the page `setup`, with `state`, the
/// co-editing state, in its data block when there is one.
pub fn page(
    blocks: &[Block],
    setup: Option<&PageSetup>,
    state: Option<&[u8]>,
    pictures: &Pictures,
) -> String {
    let title = model::first_text(blocks)
        .map(|text| text.chars().take(100).collect::<String>())
        .unwrap_or_else(|| "Untitled document".to_owned());
    write_page(blocks, setup, state, &title, pictures)
}

/// The page to print `blocks` on `setup`'s paper, titled `title`, which
/// browsers offer as the PDF's name. It has no data block.
pub fn print_page(
    blocks: &[Block],
    setup: Option<&PageSetup>,
    title: &str,
    pictures: &Pictures,
) -> String {
    write_page(blocks, setup, None, title, pictures)
}

fn write_page(
    blocks: &[Block],
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
    let colored = model::any_marks(blocks, &|marks| {
        marks.color.is_some() || marks.highlight.is_some()
    }) || model::any_cell_color(blocks);
    let _ = writeln!(
        out,
        "<style>{STYLE}{}</style>",
        if colored { "" } else { DARK_STYLE }
    );
    match setup {
        Some(setup) => {
            let _ = writeln!(out, "<style>{}</style>", page_style(setup));
            out.push_str("</head>\n<body>\n");
            let _ = writeln!(
                out,
                "<main data-page=\"{}\" data-orientation=\"{}\" data-margins=\"{}\">",
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
        None => out.push_str("</head>\n<body>\n<main>\n"),
    }
    write_blocks(&mut out, blocks, pictures);
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
pub fn fragment(blocks: &[Block], pictures: &Pictures) -> String {
    let mut out = String::new();
    write_blocks(&mut out, blocks, pictures);
    out
}

fn write_blocks(out: &mut String, blocks: &[Block], pictures: &Pictures) {
    for block in blocks {
        write_block(out, block, pictures);
    }
}

/// A paragraph's or heading's opening tag, with its layout.
fn open_para(out: &mut String, tag: &str, para: &Para) {
    let _ = write!(out, "<{tag}");
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

fn write_block(out: &mut String, block: &Block, pictures: &Pictures) {
    match block {
        Block::Paragraph(para, inlines) => {
            open_para(out, "p", para);
            write_inlines(out, inlines);
            out.push_str("</p>\n");
        }
        Block::Heading(level, para, inlines) => {
            open_para(out, &format!("h{level}"), para);
            write_inlines(out, inlines);
            let _ = writeln!(out, "</h{level}>");
        }
        Block::PageBreak => out.push_str("<div class=\"page-break\"></div>\n"),
        Block::Quote(blocks) => {
            out.push_str("<blockquote>\n");
            write_blocks(out, blocks, pictures);
            out.push_str("</blockquote>\n");
        }
        Block::Code(text) => {
            let _ = writeln!(out, "<pre><code>{}</code></pre>", escape_text(text));
        }
        Block::Rule => out.push_str("<hr>\n"),
        Block::Image(image) => write_image(out, image, pictures.get(&image.hash)),
        Block::Bullets(style, items) => {
            open_list(out, "ul", *style, None);
            for blocks in items {
                out.push_str("<li>\n");
                write_blocks(out, blocks, pictures);
                out.push_str("</li>\n");
            }
            out.push_str("</ul>\n");
        }
        Block::Numbers(start, style, items) => {
            open_list(out, "ol", *style, Some(*start));
            for blocks in items {
                out.push_str("<li>\n");
                write_blocks(out, blocks, pictures);
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
                write_blocks(out, blocks, pictures);
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
                    if let Some(color) = cell.background {
                        let _ = write!(out, " style=\"background-color:{}\"", color.css());
                    }
                    out.push_str(">\n");
                    write_blocks(out, &cell.content, pictures);
                    let _ = writeln!(out, "</{tag}>");
                }
                out.push_str("</tr>\n");
            }
            out.push_str("</tbody>\n</table>\n");
        }
    }
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
    Bold,
    Italic,
    Underline,
    Strike,
    Code,
    Super,
    Sub,
}

/// The CSS for a run's styles, such as `color:#ff0000;font-size:12pt`.
/// [`Marks::normalized`] has checked each part, so none can end the
/// attribute.
pub(crate) fn style_of(marks: &Marks) -> String {
    let mut parts = Vec::new();
    if let Some(font) = &marks.font {
        parts.push(format!("font-family:'{font}'"));
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
            Tag::Bold => out.push_str("<strong>"),
            Tag::Italic => out.push_str("<em>"),
            Tag::Underline => out.push_str("<u>"),
            Tag::Strike => out.push_str("<s>"),
            Tag::Code => out.push_str("<code>"),
            Tag::Super => out.push_str("<sup>"),
            Tag::Sub => out.push_str("<sub>"),
        }
    }

    fn close(&self, out: &mut String) {
        out.push_str(match self {
            Tag::Link(_) => "</a>",
            Tag::Style(_) => "</span>",
            Tag::Bold => "</strong>",
            Tag::Italic => "</em>",
            Tag::Underline => "</u>",
            Tag::Strike => "</s>",
            Tag::Code => "</code>",
            Tag::Super => "</sup>",
            Tag::Sub => "</sub>",
        });
    }
}

/// Text with its marks as HTML, without a block around it.
pub(crate) fn inline_fragment(inlines: &[Inline]) -> String {
    let mut out = String::new();
    write_inlines(&mut out, inlines);
    out
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
    let setup = main.and_then(|main| read_setup(&main));
    match main {
        Some(main) => read_blocks(main, &mut blocks, &mut pictures),
        None => read_blocks(*root, &mut blocks, &mut pictures),
    }
    let blocks = model::normalize(blocks);
    // Only pictures the document shows.
    let shown = model::image_hashes(&blocks);
    pictures.retain(|hash, _| shown.contains(hash));
    Document {
        blocks,
        setup,
        pictures,
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
fn read_blocks(parent: NodeRef, out: &mut Vec<Block>, pictures: &mut Pictures) {
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
                out.push(Block::Paragraph(para_of(&child), inlines_of(child)));
                images_in(child, out, pictures);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                flush(&mut runs, out);
                let level = name[1..].parse().unwrap_or(1);
                out.push(Block::Heading(level, para_of(&child), inlines_of(child)));
                images_in(child, out, pictures);
            }
            "div" if has_class(&child, "page-break") => {
                flush(&mut runs, out);
                if !break_after {
                    out.push(Block::PageBreak);
                }
            }
            "img" => {
                flush(&mut runs, out);
                out.extend(image(child, pictures));
            }
            "blockquote" => {
                flush(&mut runs, out);
                let mut blocks = Vec::new();
                read_blocks(child, &mut blocks, pictures);
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
                out.push(list(child, name == "ol", pictures));
            }
            "table" => {
                flush(&mut runs, out);
                out.push(table(child, pictures));
            }
            "br" => runs.runs.push(Run::Break),
            "div" | "section" | "article" | "main" | "header" | "footer" | "nav" | "aside"
            | "figure" | "figcaption" | "body" | "html" | "center" | "form" | "fieldset"
            | "details" | "summary" | "address" | "dl" | "dd" | "dt" | "li" | "thead" | "tbody"
            | "tfoot" | "tr" | "td" | "th" | "caption" => {
                flush(&mut runs, out);
                read_blocks(child, out, pictures);
            }
            "colgroup" | "col" => {}
            // Text elements around blocks, as Google Docs wraps what it
            // copies in a <b>, are read as a <div> would be.
            _ if holds_blocks(&child) => {
                flush(&mut runs, out);
                read_blocks(child, out, pictures);
            }
            _ => read_inline(child, &Marks::default(), &mut runs),
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
            out.extend(image(child, pictures));
        }
    }
}

/// The inline contents of a paragraph or heading, with the text styles
/// the block itself sets, as Word's web pages often do.
fn inlines_of(node: NodeRef) -> Vec<Inline> {
    let mut runs = Runs::default();
    let mut marks = Marks::default();
    apply_style(&node, &mut marks, true);
    for child in node.children() {
        read_inline(child, &marks, &mut runs);
    }
    runs.finish()
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
            marks.font = Some(face);
        }
        if let Some(size) = attribute(node, "size").and_then(style::size_from_font_tag) {
            marks.size = Some(size);
        }
    }
    let Some(css) = attribute(node, "style") else {
        return;
    };
    let declarations = style::declarations(css);
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
                    marks.font = Some(font);
                }
            }
            "font-weight" => match lower.as_str() {
                "bold" | "bolder" => marks.bold = true,
                "normal" | "lighter" => marks.bold = false,
                weight => {
                    if let Ok(weight) = weight.parse::<u32>() {
                        marks.bold = weight >= 600;
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
    apply_style(&node, &mut marks, false);
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

fn list(node: NodeRef, numbered: bool, pictures: &mut Pictures) -> Block {
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
                read_blocks(child, &mut blocks, pictures);
                items.push((is_checked(&child), blocks));
            }
            // A list right inside a list, which some programs write, goes
            // with the item before it.
            Some("ul" | "ol") => {
                let nested = list(child, element(&child) == Some("ol"), pictures);
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

fn table(node: NodeRef, pictures: &mut Pictures) -> Block {
    let mut rows = Vec::new();
    collect_rows(node, &mut rows, pictures);
    Block::Table(rows)
}

fn collect_rows(node: NodeRef, rows: &mut Vec<Vec<Cell>>, pictures: &mut Pictures) {
    for child in node.children() {
        match element(&child) {
            Some("thead" | "tbody" | "tfoot") => collect_rows(child, rows, pictures),
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
                    read_blocks(cell, &mut content, pictures);
                    cells.push(Cell {
                        header,
                        colspan: span("colspan"),
                        rowspan: span("rowspan"),
                        colwidth,
                        background: cell_color(cell),
                        content,
                    });
                }
                rows.push(cells);
            }
            _ => {}
        }
    }
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
            Block::Rule,
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
            Block::Table(vec![
                vec![Cell {
                    header: true,
                    colspan: 2,
                    rowspan: 1,
                    colwidth: Some(vec![120, 80]),
                    background: None,
                    content: vec![Block::Paragraph(Para::default(), vec![text("Head")])],
                }],
                vec![
                    Cell {
                        header: false,
                        colspan: 1,
                        rowspan: 1,
                        colwidth: None,
                        background: Some(Color([252, 229, 205])),
                        content: vec![Block::Paragraph(Para::default(), vec![text("A")])],
                    },
                    Cell {
                        header: false,
                        colspan: 1,
                        rowspan: 1,
                        colwidth: None,
                        background: None,
                        content: vec![Block::Paragraph(Para::default(), Vec::new())],
                    },
                ],
            ]),
        ]
    }

    /// Blocks with every part of a paragraph's layout, list styles, and a
    /// page break.
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
        };
        vec![Block::Table(vec![
            vec![
                cell(true, 1, Some(Color([252, 229, 205])), "Head"),
                cell(true, 1, None, "Plain"),
            ],
            vec![cell(false, 2, Some(Color([207, 226, 243])), "Wide")],
        ])]
    }

    #[test]
    fn keeps_cell_colors_and_leaves_out_dark_mode() {
        let blocks = colored_cells();
        let page = page(&blocks, None, None, &Pictures::new());
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
        let colors: Vec<_> = rows[0].iter().map(|cell| cell.background).collect();
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
        let page = page(&blocks, None, None, &Pictures::new());
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
        let page = page(&blocks, None, Some(b"state"), &Pictures::new());
        assert!(is_document(page.as_bytes()));
        assert_eq!(read(page.as_bytes()), blocks);
        assert_eq!(state_in(page.as_bytes()), Some(b"state".to_vec()));
        assert!(page.contains("<title>Trip &lt;plan&gt; &amp; more</title>"));
    }

    #[test]
    fn keeps_the_page_setup() {
        let blocks = sample();
        let setup = PageSetup {
            size: PageSize::A4,
            landscape: true,
            margins: [36.0, 72.0, 56.69, 144.0],
        };
        let written = page(&blocks, Some(&setup), None, &Pictures::new());
        assert!(written.contains("@page{size:297mm 210mm;margin:36pt 72pt 56.69pt 144pt}"));
        let read = read_document(written.as_bytes());
        assert_eq!((read.blocks, read.setup), (blocks.clone(), Some(setup)));
        // Without one, there's none to read.
        let plain = page(&blocks, None, None, &Pictures::new());
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
                Block::Table(vec![vec![cell(true, "H")], vec![cell(false, "C")]]),
            ]
        );
    }

    #[test]
    fn reads_the_styles_google_docs_and_word_write() {
        // As Google Docs puts a paste on the clipboard.
        let google = r#"<meta charset="utf-8"><b style="font-weight:normal;" id="docs-internal-guid-1"><p dir="ltr"><span style="font-size:11pt;font-family:Arial,sans-serif;color:#000000;background-color:transparent;font-weight:400;">Plain </span><span style="font-size:11pt;font-family:Arial,sans-serif;color:#ff0000;background-color:#ffff00;font-weight:700;">loud</span><span style="font-size:6.6pt;font-family:Arial,sans-serif;color:#000000;vertical-align:super;">2</span></p></b>"#;
        let arial = Marks {
            size: Some(22),
            font: Some("Arial".into()),
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
            font: Some("Times New Roman".into()),
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
