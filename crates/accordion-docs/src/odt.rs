//! A document as an OpenDocument text file (`.odt`), for **Download as**
//! (decision 30), with the same look as the Word file: Arimo 11 point,
//! the document's paper and margins, and everything the editor has, its
//! headers and footers too, in the page style, with page numbers and
//! counts as OpenDocument's fields. Footnotes are OpenDocument's. A table
//! of contents is OpenDocument's, with its entries and page numbers already
//! in it, so LibreOffice shows it as Docs does and can update it. Fonts
//! keep their own names, and a weight other than regular and bold is
//! OpenDocument's number for it.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::fonts;
use crate::model::{
    self, Align, Block, Cell, ContentsStyle, Field, Image, Inline, Kind, ListStyle, Marks,
    PageSetup, Para, Parts, Row, SIDES, Script,
};
use crate::office::{self, Grid, Part, Slot, escape, escaped};
use crate::picture::{Picture, Pictures};
use crate::style::{self, Color, HalfPoints};

/// `blocks` on `setup`'s page as an OpenDocument file, with `pictures` in
/// it and `title` in its properties. `pages` is the page each entry of a
/// table of contents is on, as the editor lays the pages out, or empty if
/// unknown.
pub fn write(
    blocks: &[Block],
    parts: &Parts,
    setup: Option<&PageSetup>,
    title: &str,
    pictures: &Pictures,
    pages: &[u32],
) -> Vec<u8> {
    let page = office::page_of(setup);
    let mut writer = Writer {
        pictures: Some(pictures),
        ..Writer::default()
    };
    if model::has_contents(blocks) {
        writer.anchors = model::heading_anchors(blocks);
        writer.contents = model::contents(blocks);
        if pages.len() == writer.contents.len() {
            writer.pages = pages.to_vec();
        }
    }
    let room = office::text_width(&page);
    writer.blocks(blocks, &Context::top(room));
    let body = std::mem::take(&mut writer.out);
    // Headers and footers have no headings the contents link to.
    writer.anchors.clear();
    // Headers and footers come from the same writer, so their styles have
    // names of their own, and go in the styles file too.
    let mut margins = BTreeMap::new();
    for (name, blocks) in parts.each() {
        if !blocks.is_empty() {
            writer.blocks(blocks, &Context::top(room));
            margins.insert(name, std::mem::take(&mut writer.out));
        }
    }
    let mut automatic = String::new();
    let _ = write!(automatic, "{CONTENT_STYLES}{}", lists());
    for (key, name) in &writer.spans {
        let _ = write!(
            automatic,
            "<style:style style:name=\"{name}\" style:family=\"text\"><style:text-properties{}/></style:style>",
            key.properties()
        );
    }
    for depth in 1..=writer.deepest_quote {
        let _ = write!(
            automatic,
            "<style:style style:name=\"Quote{depth}\" style:family=\"paragraph\" style:parent-style-name=\"Quotations\">\
             <style:paragraph-properties fo:margin-left=\"{}pt\"/></style:style>",
            depth * 18
        );
    }
    for (key, name) in &writer.paras {
        let _ = write!(
            automatic,
            "<style:style style:name=\"{name}\" style:family=\"paragraph\" style:parent-style-name=\"{}\">\
             <style:paragraph-properties{}/></style:style>",
            key.0, key.1
        );
    }
    automatic.push_str(&writer.styles);
    let mut content = String::from(XML);
    let _ = write!(
        content,
        "<office:document-content {NAMESPACES} office:version=\"1.3\">{FONTS}<office:automatic-styles>{automatic}\
         </office:automatic-styles><office:body><office:text>{body}</office:text></office:body></office:document-content>"
    );

    let (width, height) = page.points();
    // A header sits half an inch from the paper's top, as in Google Docs
    // and Word, and the text starts at the margin below it unless the
    // header is taller. OpenDocument puts headers inside the margins, so
    // the margin becomes the half inch and the header takes the rest.
    let has = |a: &str, b: &str| margins.contains_key(a) || margins.contains_key(b);
    let [mut top, right, mut bottom, left] = page.margins;
    let mut header_style = String::new();
    let mut footer_style = String::new();
    if has("header", "first_header") && top > HEADER_FROM {
        let _ = write!(
            header_style,
            "<style:header-style><style:header-footer-properties fo:min-height=\"{}pt\" fo:margin-bottom=\"0pt\" \
             style:dynamic-spacing=\"true\"/></style:header-style>",
            office::number(top - HEADER_FROM)
        );
        top = HEADER_FROM;
    }
    if has("footer", "first_footer") && bottom > HEADER_FROM {
        let _ = write!(
            footer_style,
            "<style:footer-style><style:header-footer-properties fo:min-height=\"{}pt\" fo:margin-top=\"0pt\" \
             style:dynamic-spacing=\"true\"/></style:footer-style>",
            office::number(bottom - HEADER_FROM)
        );
        bottom = HEADER_FROM;
    }
    let [top, right, bottom, left] = [top, right, bottom, left].map(office::number);
    let mut master = String::new();
    for (name, tag) in [
        ("header", "header"),
        ("first_header", "header-first"),
        ("footer", "footer"),
        ("first_footer", "footer-first"),
    ] {
        match margins.get(name) {
            Some(xml) => {
                let _ = write!(master, "<style:{tag}>{xml}</style:{tag}>");
            }
            // A first page without its own shows an empty one. LibreOffice
            // hides every header for `style:display="false"`.
            None if parts.different_first && tag.ends_with("first") => {
                let _ = write!(master, "<style:{tag}><text:p/></style:{tag}>");
            }
            None => {}
        }
    }
    let styles = format!(
        "{XML}<office:document-styles {NAMESPACES} office:version=\"1.3\">{FONTS}{STYLES}\
         <office:automatic-styles>{automatic}<style:page-layout style:name=\"Page\">\
         <style:page-layout-properties fo:page-width=\"{}pt\" fo:page-height=\"{}pt\" style:print-orientation=\"{}\" \
         fo:margin-top=\"{top}pt\" fo:margin-right=\"{right}pt\" fo:margin-bottom=\"{bottom}pt\" fo:margin-left=\"{left}pt\"/>\
         {header_style}{footer_style}</style:page-layout></office:automatic-styles>\
         <office:master-styles><style:master-page style:name=\"Standard\" style:page-layout-name=\"Page\">{master}\
         </style:master-page></office:master-styles></office:document-styles>",
        office::number(width),
        office::number(height),
        if page.landscape {
            "landscape"
        } else {
            "portrait"
        },
    );
    let meta = format!(
        "{XML}<office:document-meta {NAMESPACES} office:version=\"1.3\"><office:meta>\
         <dc:title>{}</dc:title></office:meta></office:document-meta>",
        escaped(title)
    );
    let media_paths: Vec<String> = writer.media.iter().map(media_path).collect();
    let mut manifest = MANIFEST.to_owned();
    let entries: String = media_paths
        .iter()
        .zip(&writer.media)
        .map(|(path, picture)| {
            format!(
                "<manifest:file-entry manifest:full-path=\"{path}\" manifest:media-type=\"{}\"/>",
                picture.content_type
            )
        })
        .collect();
    manifest = manifest.replace(
        "</manifest:manifest>",
        &format!("{entries}</manifest:manifest>"),
    );
    let mut parts = vec![
        // First and stored as is, so a program can tell what the file is
        // from its first bytes.
        Part {
            path: "mimetype",
            contents: MIME_TYPE.as_bytes(),
            stored: true,
        },
        Part {
            path: "META-INF/manifest.xml",
            contents: manifest.as_bytes(),
            stored: false,
        },
        Part {
            path: "content.xml",
            contents: content.as_bytes(),
            stored: false,
        },
        Part {
            path: "styles.xml",
            contents: styles.as_bytes(),
            stored: false,
        },
        Part {
            path: "meta.xml",
            contents: meta.as_bytes(),
            stored: false,
        },
    ];
    for (path, picture) in media_paths.iter().zip(&writer.media) {
        parts.push(Part {
            path,
            contents: &picture.contents,
            // Pictures are compressed already.
            stored: true,
        });
    }
    office::pack(&parts)
}

/// Where a picture goes in the package, named by its hash.
/// How far headers and footers are from the paper's edge, in points.
const HEADER_FROM: f64 = 36.0;

fn media_path(picture: &Picture) -> String {
    format!("Pictures/{}.{}", picture.hash, picture.extension())
}

pub const MIME_TYPE: &str = "application/vnd.oasis.opendocument.text";

const XML: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n";

const NAMESPACES: &str = "xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" \
xmlns:style=\"urn:oasis:names:tc:opendocument:xmlns:style:1.0\" \
xmlns:text=\"urn:oasis:names:tc:opendocument:xmlns:text:1.0\" \
xmlns:table=\"urn:oasis:names:tc:opendocument:xmlns:table:1.0\" \
xmlns:fo=\"urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0\" \
xmlns:svg=\"urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0\" \
xmlns:draw=\"urn:oasis:names:tc:opendocument:xmlns:drawing:1.0\" \
xmlns:xlink=\"http://www.w3.org/1999/xlink\" \
xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
xmlns:meta=\"urn:oasis:names:tc:opendocument:xmlns:meta:1.0\"";

/// The marks a span of text has, which name its style.
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
struct SpanKey {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    code: bool,
    /// A done checklist item's text: gray.
    gray: bool,
    script: Option<Script>,
    color: Option<Color>,
    highlight: Option<Color>,
    size: Option<HalfPoints>,
    font: Option<String>,
    weight: Option<u16>,
}

impl SpanKey {
    fn properties(&self) -> String {
        let mut out = String::new();
        match fonts::shown_weight(self.weight, self.bold) {
            400 => {}
            700 => out.push_str(" fo:font-weight=\"bold\""),
            weight => {
                let _ = write!(out, " fo:font-weight=\"{weight}\"");
            }
        }
        if self.italic {
            out.push_str(" fo:font-style=\"italic\"");
        }
        if self.underline {
            out.push_str(
                " style:text-underline-style=\"solid\" style:text-underline-width=\"auto\" style:text-underline-color=\"font-color\"",
            );
        }
        if self.strike {
            out.push_str(
                " style:text-line-through-style=\"solid\" style:text-line-through-type=\"single\"",
            );
        }
        if let Some(font) = &self.font {
            let _ = write!(out, " fo:font-family=\"&apos;{}&apos;\"", escaped(font));
        } else if self.code {
            out.push_str(" style:font-name=\"Cousine\"");
        }
        if let Some(color) = self.highlight {
            let _ = write!(out, " fo:background-color=\"{}\"", color.css());
        } else if self.code {
            out.push_str(" fo:background-color=\"#f1f3f4\"");
        }
        if self.gray {
            out.push_str(" fo:color=\"#5f6368\"");
        } else if let Some(color) = self.color {
            let _ = write!(out, " fo:color=\"{}\"", color.css());
        }
        if let Some(size) = self.size {
            let _ = write!(out, " fo:font-size=\"{}pt\"", style::points_text(size));
        }
        match self.script {
            Some(Script::Super) => out.push_str(" style:text-position=\"super 58%\""),
            Some(Script::Sub) => out.push_str(" style:text-position=\"sub 58%\""),
            None => {}
        }
        out
    }
}

#[derive(Clone)]
struct Context {
    /// How many quotes deep.
    quotes: usize,
    /// In a list.
    listed: bool,
    /// Struck through and gray: a done checklist item's text.
    done: bool,
    /// In a table cell, and whether it's a heading cell.
    cell: Option<bool>,
    /// The room for text across, in points, for tables.
    room: f64,
}

impl Context {
    fn top(room: f64) -> Self {
        Self {
            quotes: 0,
            listed: false,
            done: false,
            cell: None,
            room,
        }
    }
}

#[derive(Default)]
struct Writer<'a> {
    out: String,
    pictures: Option<&'a Pictures>,
    /// Each picture in the file, once.
    media: Vec<Picture>,
    /// How many pictures are drawn, for their names.
    drawings: usize,
    /// Each mix of marks used, and its style's name.
    spans: BTreeMap<SpanKey, String>,
    /// Each paragraph layout used, by its parent style and properties, and
    /// its style's name.
    paras: BTreeMap<(String, String), String>,
    /// Table, column, and cell styles, written as tables are.
    styles: String,
    /// Each colored cell style used, by whether it's a heading cell and
    /// its color, and its style's name.
    /// Styles of a document's own, by their properties: cells' and
    /// lines'.
    cells: BTreeMap<String, String>,
    rules: BTreeMap<String, String>,
    tables: usize,
    deepest_quote: usize,
    /// Whether the text written next starts a line, where a space would
    /// be dropped.
    line_start: bool,
    /// How many footnotes so far, for their numbers.
    notes: usize,
    /// Each heading's anchor, while the document has a table of contents,
    /// and how many headings are written.
    anchors: Vec<Option<String>>,
    headings: usize,
    /// The table of contents' entries, and each one's page, if known.
    contents: Vec<model::Entry>,
    pages: Vec<u32>,
}

impl Writer<'_> {
    /// A cell's style: the plain or heading cell's, or one of its own
    /// with its color, borders, or padding.
    fn cell_style(&mut self, cell: &Cell) -> String {
        let base = if cell.header { "HeadingCell" } else { "Cell" };
        if cell.background.is_none() && cell.borders == [None; 4] && cell.padding.is_none() {
            return base.to_owned();
        }
        let mut props = String::new();
        let background = match cell.background {
            Some(color) => Some(color.css()),
            None if cell.header => Some("#f1f3f4".to_owned()),
            None => None,
        };
        if let Some(background) = background {
            let _ = write!(props, " fo:background-color=\"{background}\"");
        }
        match cell.padding {
            Some(padding) => {
                for (side, length) in SIDES.iter().zip(padding) {
                    let _ = write!(
                        props,
                        " fo:padding-{side}=\"{}pt\"",
                        office::number(model::points(length))
                    );
                }
            }
            None => props.push_str(" fo:padding=\"4pt\""),
        }
        if cell.borders == [None; 4] {
            props.push_str(" fo:border=\"0.75pt solid #c4c7c5\"");
        } else {
            let color = cell.border_color.unwrap_or(Color::BLACK).css();
            for (side, border) in SIDES.iter().zip(cell.borders) {
                let line = match border {
                    None => "0.75pt solid #c4c7c5".to_owned(),
                    Some(0) => "none".to_owned(),
                    Some(border) => {
                        format!("{}pt solid {color}", office::number(model::points(border)))
                    }
                };
                let _ = write!(props, " fo:border-{side}=\"{line}\"");
            }
        }
        let count = self.cells.len();
        let styles = &mut self.styles;
        self.cells
            .entry(format!("{base}{props}"))
            .or_insert_with(|| {
                let name = format!("{base}{}", count + 1);
                let _ = write!(
                    styles,
                    "<style:style style:name=\"{name}\" style:family=\"table-cell\">\
                     <style:table-cell-properties{props}/></style:style>"
                );
                name
            })
            .clone()
    }

    /// A line's style: the usual one, or one of its own with its
    /// thickness or color.
    fn rule_style(&mut self, rule: &model::Rule) -> String {
        if rule.thickness.is_none() && rule.color.is_none() {
            return "Rule".to_owned();
        }
        let thickness = rule.thickness.map_or("0.75".to_owned(), |thickness| {
            office::number(model::points(thickness))
        });
        let color = rule.color.map_or("#c4c7c5".to_owned(), Color::css);
        let border = format!("{thickness}pt solid {color}");
        let count = self.rules.len();
        let styles = &mut self.styles;
        self.rules
            .entry(border.clone())
            .or_insert_with(|| {
                let name = format!("Rule{}", count + 1);
                let _ = write!(
                    styles,
                    "<style:style style:name=\"{name}\" style:family=\"paragraph\" style:parent-style-name=\"Rule\">\
                     <style:paragraph-properties fo:border-bottom=\"{border}\"/></style:style>"
                );
                name
            })
            .clone()
    }

    fn blocks(&mut self, blocks: &[Block], context: &Context) {
        for block in blocks {
            self.block(block, context);
        }
    }

    fn paragraph_style(&mut self, context: &Context) -> String {
        match context.cell {
            Some(true) => "Table_20_Heading".to_owned(),
            Some(false) => "Table_20_Contents".to_owned(),
            None if context.quotes > 0 => {
                self.deepest_quote = self.deepest_quote.max(context.quotes);
                format!("Quote{}", context.quotes)
            }
            None if context.listed => "ListText".to_owned(),
            None => "Standard".to_owned(),
        }
    }

    /// The style for a paragraph with `para`'s layout where `base` would
    /// be its style: `base` itself when it has none of its own.
    fn layout_style(&mut self, base: String, para: &Para, context: &Context) -> String {
        // The title and subtitle are styles of their own already.
        let layout = Para {
            kind: None,
            ..para.clone()
        };
        if layout.is_default() {
            return base;
        }
        // A style of a document's own can't be another's parent, so those
        // are written out in full.
        let mut props = String::new();
        let (parent, quote_indent) = if base.starts_with("Quote") {
            ("Quotations".to_owned(), context.quotes as f64 * 18.0)
        } else if base == "ListText" {
            if para.after.is_none() {
                props.push_str(" fo:margin-bottom=\"2pt\"");
            }
            ("Standard".to_owned(), 0.0)
        } else {
            (base, 0.0)
        };
        let points = |twips| office::number(model::points(twips));
        match para.align {
            Align::Left => {}
            Align::Center => props.push_str(" fo:text-align=\"center\""),
            Align::Right => props.push_str(" fo:text-align=\"end\""),
            Align::Justify => props.push_str(" fo:text-align=\"justify\""),
        }
        // In a list, the list places the text.
        if !context.listed && (para.indent != 0 || quote_indent > 0.0) {
            let _ = write!(
                props,
                " fo:margin-left=\"{}pt\"",
                office::number(quote_indent + model::points(para.indent))
            );
        }
        if para.indent_right != 0 {
            let _ = write!(
                props,
                " fo:margin-right=\"{}pt\"",
                points(para.indent_right)
            );
        }
        if para.first_line != 0 {
            let _ = write!(props, " fo:text-indent=\"{}pt\"", points(para.first_line));
        }
        if let Some(line) = para.line {
            let _ = write!(props, " fo:line-height=\"{line}%\"");
        }
        if let Some(before) = para.before {
            let _ = write!(props, " fo:margin-top=\"{}pt\"", points(before));
        }
        if let Some(after) = para.after {
            let _ = write!(props, " fo:margin-bottom=\"{}pt\"", points(after));
        }
        let count = self.paras.len();
        self.paras
            .entry((parent, props))
            .or_insert_with(|| format!("Layout{}", count + 1))
            .clone()
    }

    fn block(&mut self, block: &Block, context: &Context) {
        match block {
            Block::Paragraph(para, inlines) => {
                let base = match para.kind {
                    // A title in a list or a cell takes the list's or the
                    // cell's spacing.
                    Some(Kind::Title) if context.cell.is_none() && context.quotes == 0 => {
                        "Title".to_owned()
                    }
                    Some(Kind::Subtitle) if context.cell.is_none() && context.quotes == 0 => {
                        "Subtitle".to_owned()
                    }
                    _ => self.paragraph_style(context),
                };
                let style = self.layout_style(base, para, context);
                self.open(&format!("<text:p text:style-name=\"{style}\">"));
                self.runs(inlines, context);
                self.out.push_str("</text:p>");
            }
            Block::Heading(level, para, inlines) => {
                let level = (*level).clamp(1, 6);
                let style = self.layout_style(format!("Heading_20_{level}"), para, context);
                self.open(&format!(
                    "<text:h text:style-name=\"{style}\" text:outline-level=\"{level}\">"
                ));
                let mark = self.bookmark();
                if let Some(name) = &mark {
                    let _ = write!(self.out, "<text:bookmark-start text:name=\"{name}\"/>");
                }
                self.runs(inlines, context);
                if let Some(name) = &mark {
                    let _ = write!(self.out, "<text:bookmark-end text:name=\"{name}\"/>");
                }
                self.out.push_str("</text:h>");
            }
            Block::Contents(style) => self.contents(*style, context),
            Block::PageBreak => self.out.push_str("<text:p text:style-name=\"PageBreak\"/>"),
            Block::Quote(blocks) => {
                let mut inner = context.clone();
                inner.quotes += 1;
                self.blocks(blocks, &inner);
            }
            Block::Code(text) => {
                for line in text.strip_suffix('\n').unwrap_or(text).split('\n') {
                    self.open("<text:p text:style-name=\"Preformatted_20_Text\">");
                    self.text(line);
                    self.out.push_str("</text:p>");
                }
            }
            Block::Rule(rule) => {
                let style = self.rule_style(rule);
                let _ = write!(self.out, "<text:p text:style-name=\"{style}\"/>");
            }
            Block::Bullets(style, items) => {
                let name = style.map_or("Bullets".to_owned(), list_style_name);
                let _ = write!(self.out, "<text:list text:style-name=\"{name}\">");
                for item in items {
                    self.item(item, context, "");
                }
                self.out.push_str("</text:list>");
            }
            Block::Numbers(start, style, items) => {
                let name = style.map_or("Numbers".to_owned(), list_style_name);
                let _ = write!(self.out, "<text:list text:style-name=\"{name}\">");
                for (index, item) in items.iter().enumerate() {
                    let start = if index == 0 && *start != 1 {
                        format!(" text:start-value=\"{start}\"")
                    } else {
                        String::new()
                    };
                    self.item(item, context, &start);
                }
                self.out.push_str("</text:list>");
            }
            Block::Checks(items) => {
                // A list of its own for each item, since each item's box
                // shows whether it's checked.
                for (checked, item) in items {
                    let style = if *checked { "Checked" } else { "Unchecked" };
                    let _ = write!(self.out, "<text:list text:style-name=\"{style}\">");
                    let mut inner = context.clone();
                    inner.done = *checked;
                    self.item(item, &inner, "");
                    self.out.push_str("</text:list>");
                }
            }
            Block::Table(rows) => self.table(rows, context),
            Block::Image(image) => self.image(image, context),
        }
    }

    /// A picture in a paragraph of its own, as large as it's shown on the
    /// page. One whose contents are missing is left out.
    fn image(&mut self, image: &Image, context: &Context) {
        let Some(picture) = self.pictures.and_then(|pictures| pictures.get(&image.hash)) else {
            return;
        };
        if !self.media.iter().any(|kept| kept.hash == picture.hash) {
            self.media.push(picture.clone());
        }
        self.drawings += 1;
        let room = context.room - context.quotes as f64 * 18.0;
        let (width, height) = office::picture_size(picture, image.width, room);
        let style = self.paragraph_style(context);
        let _ = write!(
            self.out,
            "<text:p text:style-name=\"{style}\"><draw:frame draw:name=\"Picture {}\" text:anchor-type=\"as-char\" \
             svg:width=\"{}pt\" svg:height=\"{}pt\"><draw:image xlink:href=\"{}\" xlink:type=\"simple\" \
             xlink:show=\"embed\" xlink:actuate=\"onLoad\"/>",
            self.drawings,
            office::number(width),
            office::number(height),
            media_path(picture),
        );
        if !image.alt.is_empty() {
            let _ = write!(self.out, "<svg:desc>{}</svg:desc>", escaped(&image.alt));
        }
        self.out.push_str("</draw:frame></text:p>");
    }

    fn item(&mut self, blocks: &[Block], context: &Context, attributes: &str) {
        let _ = write!(self.out, "<text:list-item{attributes}>");
        if blocks.is_empty() {
            self.out.push_str("<text:p/>");
        }
        let mut inner = context.clone();
        inner.listed = true;
        for block in blocks {
            self.block(block, &inner);
            // Only the item's own text is struck through.
            inner.done = false;
        }
        self.out.push_str("</text:list-item>");
    }

    fn runs(&mut self, inlines: &[Inline], context: &Context) {
        let mut link: Option<&str> = None;
        for inline in inlines {
            let marks = match inline {
                Inline::Text(text, marks) if !text.is_empty() => Some(marks),
                Inline::Text(..) => continue,
                Inline::Break | Inline::Footnote(_) => None,
            };
            let href = marks.and_then(|marks| marks.link.as_deref());
            if link.is_some() && href != link {
                self.out.push_str("</text:a>");
                link = None;
            }
            if let (Some(href), None) = (href, link) {
                let _ = write!(
                    self.out,
                    "<text:a xlink:type=\"simple\" xlink:href=\"{}\" text:style-name=\"Internet_20_link\">",
                    escaped(href)
                );
                link = Some(href);
            }
            match inline {
                Inline::Text(text, marks) => self.span(text, marks, context),
                Inline::Break => self.open("<text:line-break/>"),
                Inline::Footnote(text) => self.footnote(text, context),
            }
        }
        if link.is_some() {
            self.out.push_str("</text:a>");
        }
    }

    /// A footnote, with its text, where its number goes.
    fn footnote(&mut self, text: &[Inline], context: &Context) {
        self.notes += 1;
        let number = self.notes;
        let line_start = self.line_start;
        let _ = write!(
            self.out,
            "<text:note text:id=\"ftn{number}\" text:note-class=\"footnote\">\
             <text:note-citation>{number}</text:note-citation><text:note-body>"
        );
        self.open("<text:p text:style-name=\"Footnote\">");
        self.runs(text, &Context::top(context.room));
        self.out.push_str("</text:p></text:note-body></text:note>");
        self.line_start = line_start;
    }

    fn span(&mut self, text: &str, marks: &Marks, context: &Context) {
        let key = SpanKey {
            bold: marks.bold,
            italic: marks.italic,
            underline: marks.underline,
            strike: marks.strike || context.done,
            code: marks.code,
            gray: context.done,
            script: marks.script,
            color: marks.color,
            highlight: marks.highlight,
            size: marks.size,
            font: marks.font.clone(),
            weight: marks.weight,
        };
        if key == SpanKey::default() {
            self.field_or_text(text, marks);
            return;
        }
        let count = self.spans.len();
        let name = self
            .spans
            .entry(key)
            .or_insert_with(|| format!("Span{}", count + 1))
            .clone();
        let _ = write!(self.out, "<text:span text:style-name=\"{name}\">");
        self.field_or_text(text, marks);
        self.out.push_str("</text:span>");
    }

    /// A page number or count as OpenDocument's field, or the text.
    fn field_or_text(&mut self, text: &str, marks: &Marks) {
        match marks.field {
            Some(Field::PageNumber) => self
                .out
                .push_str("<text:page-number text:select-page=\"current\">1</text:page-number>"),
            Some(Field::PageCount) => self.out.push_str("<text:page-count>1</text:page-count>"),
            None => self.text(text),
        }
        self.line_start = false;
    }

    /// Writes text as OpenDocument keeps it. Spaces in the XML collapse
    /// into one, and go at the start of a line, so a run of them is one
    /// space and a counted `<text:s>`, and tabs and line ends are elements.
    fn text(&mut self, text: &str) {
        let mut spaces = 0;
        let mut one = [0; 4];
        for c in text.chars() {
            if c == ' ' {
                spaces += 1;
                continue;
            }
            self.spaces(spaces);
            spaces = 0;
            match c {
                '\t' => self.out.push_str("<text:tab/>"),
                '\n' => self.out.push_str("<text:line-break/>"),
                c => escape(&mut self.out, c.encode_utf8(&mut one)),
            }
            self.line_start = c == '\t' || c == '\n';
        }
        self.spaces(spaces);
    }

    fn spaces(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        let kept = if self.line_start {
            count
        } else {
            self.out.push(' ');
            count - 1
        };
        match kept {
            0 => {}
            1 => self.out.push_str("<text:s/>"),
            kept => {
                let _ = write!(self.out, "<text:s text:c=\"{kept}\"/>");
            }
        }
        self.line_start = false;
    }

    /// Opens a paragraph or heading.
    /// The bookmark the next heading's table of contents entry links to,
    /// if it has one.
    fn bookmark(&mut self) -> Option<String> {
        let index = self.headings;
        self.headings += 1;
        let anchor = self.anchors.get(index).cloned().flatten()?;
        let entry = self
            .contents
            .iter()
            .position(|entry| entry.anchor == anchor)?;
        Some(toc_name(entry))
    }

    /// A table of contents: its source, which says how LibreOffice makes it
    /// again, and a paragraph for each heading with a link to its bookmark,
    /// as LibreOffice writes one.
    fn contents(&mut self, style: ContentsStyle, context: &Context) {
        let number = self.out.matches("<text:table-of-content ").count() + 1;
        let _ = write!(
            self.out,
            "<text:table-of-content text:name=\"Table of Contents{number}\" text:protected=\"false\">\
             <text:table-of-content-source text:outline-level=\"6\" text:use-index-marks=\"false\" text:use-outline-level=\"true\">"
        );
        for level in 1..=6 {
            let _ = write!(
                self.out,
                "<text:table-of-content-entry-template text:outline-level=\"{level}\" text:style-name=\"Contents_20_{level}\">\
                 <text:index-entry-link-start{}/><text:index-entry-text/>{}<text:index-entry-link-end/>\
                 </text:table-of-content-entry-template>",
                if style.numbered() {
                    ""
                } else {
                    " text:style-name=\"Internet_20_link\""
                },
                match style {
                    ContentsStyle::Links => "",
                    ContentsStyle::Dotted =>
                        "<text:index-entry-tab-stop style:type=\"right\" style:leader-char=\".\"/><text:index-entry-page-number/>",
                    ContentsStyle::Plain =>
                        "<text:index-entry-tab-stop style:type=\"right\" style:leader-char=\" \"/><text:index-entry-page-number/>",
                }
            );
        }
        self.out
            .push_str("</text:table-of-content-source><text:index-body>");
        let entries = std::mem::take(&mut self.contents);
        for (index, entry) in entries.iter().enumerate() {
            let paragraph = self.toc_style(entry.level, style, context.room);
            let _ = write!(
                self.out,
                "<text:p text:style-name=\"{paragraph}\"><text:a xlink:type=\"simple\" xlink:href=\"#{}\"{}>",
                toc_name(index),
                if style.numbered() {
                    ""
                } else {
                    " text:style-name=\"Internet_20_link\""
                },
            );
            escape(&mut self.out, &entry.text);
            if style.numbered() {
                self.out.push_str("<text:tab/>");
                if let Some(page) = self.pages.get(index) {
                    let _ = write!(self.out, "{page}");
                }
            }
            self.out.push_str("</text:a></text:p>");
        }
        self.contents = entries;
        self.out
            .push_str("</text:index-body></text:table-of-content>");
    }

    /// The style of an entry at `level`, with a tab stop at the right
    /// margin for its page number, its dots leading to it if `style` has
    /// them.
    fn toc_style(&mut self, level: u8, style: ContentsStyle, room: f64) -> String {
        if !style.numbered() {
            return format!("Contents_20_{level}");
        }
        let name = format!(
            "Toc{}{level}",
            if style == ContentsStyle::Dotted {
                "Dotted"
            } else {
                "Plain"
            }
        );
        if self.styles.contains(&format!("style:name=\"{name}\"")) {
            return name;
        }
        let leader = if style == ContentsStyle::Dotted {
            " style:leader-style=\"dotted\" style:leader-text=\".\""
        } else {
            ""
        };
        let _ = write!(
            self.styles,
            "<style:style style:name=\"{name}\" style:family=\"paragraph\" style:parent-style-name=\"Contents_20_{level}\">\
             <style:paragraph-properties><style:tab-stops><style:tab-stop style:position=\"{}pt\" style:type=\"right\"{leader}/>\
             </style:tab-stops></style:paragraph-properties></style:style>",
            office::number(room - f64::from(level - 1) * 18.0)
        );
        name
    }

    fn open(&mut self, tag: &str) {
        self.out.push_str(tag);
        self.line_start = true;
    }

    fn table(&mut self, rows: &[Row], context: &Context) {
        let grid = Grid::new(
            rows,
            (context.room - context.quotes as f64 * 18.0).max(72.0),
        );
        self.tables += 1;
        let name = format!("Table{}", self.tables);
        let total: f64 = grid.widths.iter().sum();
        let _ = write!(
            self.styles,
            "<style:style style:name=\"{name}\" style:family=\"table\">\
             <style:table-properties style:width=\"{}pt\" table:align=\"left\" table:border-model=\"collapsing\" fo:margin-bottom=\"6pt\"/></style:style>",
            office::number(total)
        );
        for (index, width) in grid.widths.iter().enumerate() {
            let _ = write!(
                self.styles,
                "<style:style style:name=\"{name}.C{index}\" style:family=\"table-column\">\
                 <style:table-column-properties style:column-width=\"{}pt\"/></style:style>",
                office::number(*width)
            );
        }
        let _ = write!(
            self.out,
            "<table:table table:name=\"{name}\" table:style-name=\"{name}\">"
        );
        for index in 0..grid.columns {
            let _ = write!(
                self.out,
                "<table:table-column table:style-name=\"{name}.C{index}\"/>"
            );
        }
        // Rows of heading cells at the top repeat on each page.
        let heading_rows = rows
            .iter()
            .take_while(|row| !row.cells.is_empty() && row.cells.iter().all(|cell| cell.header))
            .count()
            .min(rows.len().saturating_sub(1));
        for (index, row) in grid.rows.iter().enumerate() {
            if index == 0 && heading_rows > 0 {
                self.out.push_str("<table:table-header-rows>");
            }
            match rows[index].height {
                Some(height) => {
                    let _ = write!(
                        self.styles,
                        "<style:style style:name=\"{name}.R{index}\" style:family=\"table-row\">\
                         <style:table-row-properties style:min-row-height=\"{}pt\"/></style:style>",
                        office::number(model::points(height))
                    );
                    let _ = write!(
                        self.out,
                        "<table:table-row table:style-name=\"{name}.R{index}\">"
                    );
                }
                None => self.out.push_str("<table:table-row>"),
            }
            let mut column = 0;
            for slot in row {
                match slot {
                    Slot::Cell { cell, span, rows } => {
                        let style = self.cell_style(cell);
                        let _ = write!(
                            self.out,
                            "<table:table-cell table:style-name=\"{style}\" office:value-type=\"string\""
                        );
                        if *span > 1 {
                            let _ = write!(self.out, " table:number-columns-spanned=\"{span}\"");
                        }
                        if *rows > 1 {
                            let _ = write!(self.out, " table:number-rows-spanned=\"{rows}\"");
                        }
                        self.out.push('>');
                        let across = cell.padding.map_or(8.0, |padding| {
                            model::points(padding[1]) + model::points(padding[3])
                        });
                        let mut inner = Context::top(grid.width(column, *span) - across);
                        inner.cell = Some(cell.header);
                        if cell.content.is_empty() {
                            self.out.push_str("<text:p/>");
                        }
                        self.blocks(&cell.content, &inner);
                        self.out.push_str("</table:table-cell>");
                        for _ in 1..*span {
                            self.out.push_str("<table:covered-table-cell/>");
                        }
                        column += span;
                    }
                    Slot::Below { span } => {
                        for _ in 0..*span {
                            self.out.push_str("<table:covered-table-cell/>");
                        }
                        column += span;
                    }
                    Slot::Empty => {
                        self.out.push_str(
                            "<table:table-cell table:style-name=\"Cell\" office:value-type=\"string\"><text:p/></table:table-cell>",
                        );
                        column += 1;
                    }
                }
            }
            self.out.push_str("</table:table-row>");
            if index + 1 == heading_rows {
                self.out.push_str("</table:table-header-rows>");
            }
        }
        self.out.push_str("</table:table>");
    }
}

/// Styles that LibreOffice reads only among a document's own: table cells,
/// and paragraphs in lists, which sit closer together.
const CONTENT_STYLES: &str = "<style:style style:name=\"Cell\" style:family=\"table-cell\">\
<style:table-cell-properties fo:padding=\"4pt\" fo:border=\"0.75pt solid #c4c7c5\"/></style:style>\
<style:style style:name=\"HeadingCell\" style:family=\"table-cell\">\
<style:table-cell-properties fo:background-color=\"#f1f3f4\" fo:padding=\"4pt\" fo:border=\"0.75pt solid #c4c7c5\"/></style:style>\
<style:style style:name=\"ListText\" style:family=\"paragraph\" style:parent-style-name=\"Standard\">\
<style:paragraph-properties fo:margin-bottom=\"2pt\"/></style:style>\
<style:style style:name=\"Footnote\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:class=\"extra\">\
<style:paragraph-properties fo:margin-top=\"0pt\" fo:margin-bottom=\"0pt\" fo:line-height=\"100%\"/>\
<style:text-properties fo:font-size=\"10pt\"/></style:style>\
<style:style style:name=\"PageBreak\" style:family=\"paragraph\" style:parent-style-name=\"Standard\">\
<style:paragraph-properties fo:break-after=\"page\" fo:margin-top=\"0pt\" fo:margin-bottom=\"0pt\" fo:line-height=\"100%\"/>\
<style:text-properties fo:font-size=\"1pt\"/></style:style>";

const FONTS: &str = "<office:font-face-decls>\
<style:font-face style:name=\"Arimo\" svg:font-family=\"Arimo\" style:font-family-generic=\"swiss\" style:font-pitch=\"variable\"/>\
<style:font-face style:name=\"Cousine\" svg:font-family=\"Cousine\" style:font-family-generic=\"modern\" style:font-pitch=\"fixed\"/>\
<style:font-face style:name=\"Segoe UI Symbol\" svg:font-family=\"&apos;Segoe UI Symbol&apos;\" style:font-pitch=\"variable\"/>\
</office:font-face-decls>";

/// The list styles: bullets, numbers, and the two checklist boxes, each to
/// ten levels. Numbers are 1, 2, 3 at every level, as browsers show them. Each level's text is half an inch further in, with its label
/// hanging a quarter inch to its left.
fn lists() -> String {
    let mut out = String::new();
    let bullets: [(&str, [&str; 3]); 3] = [
        ("Bullets", ["•", "◦", "▪"]),
        ("Unchecked", ["☐"; 3]),
        ("Checked", ["☑"; 3]),
    ];
    for (name, symbols) in bullets {
        let _ = write!(out, "<text:list-style style:name=\"{name}\">");
        for level in 1..=10 {
            let _ = write!(
                out,
                "<text:list-level-style-bullet text:level=\"{level}\" text:bullet-char=\"{}\">{}</text:list-level-style-bullet>",
                symbols[(level - 1) % 3],
                level_properties(level)
            );
        }
        out.push_str("</text:list-style>");
    }
    for style in ListStyle::ALL {
        let _ = write!(
            out,
            "<text:list-style style:name=\"{}\">",
            list_style_name(style)
        );
        for level in 1..=10 {
            match style.bullet() {
                Some(bullet) => {
                    let _ = write!(
                        out,
                        "<text:list-level-style-bullet text:level=\"{level}\" text:bullet-char=\"{bullet}\">{}</text:list-level-style-bullet>",
                        level_properties(level)
                    );
                }
                None => {
                    let (format, suffix) = match style {
                        ListStyle::DecimalParen => ("1", ")"),
                        ListStyle::LowerAlpha => ("a", "."),
                        ListStyle::UpperAlpha => ("A", "."),
                        ListStyle::LowerRoman => ("i", "."),
                        ListStyle::UpperRoman => ("I", "."),
                        _ => ("1", "."),
                    };
                    let _ = write!(
                        out,
                        "<text:list-level-style-number text:level=\"{level}\" style:num-format=\"{format}\" style:num-suffix=\"{suffix}\">{}</text:list-level-style-number>",
                        level_properties(level)
                    );
                }
            }
        }
        out.push_str("</text:list-style>");
    }
    out.push_str("<text:list-style style:name=\"Numbers\">");
    for level in 1..=10 {
        let _ = write!(
            out,
            "<text:list-level-style-number text:level=\"{level}\" style:num-format=\"1\" style:num-suffix=\".\">{}</text:list-level-style-number>",
            level_properties(level)
        );
    }
    out.push_str("</text:list-style>");
    out
}

/// The list style for a [`ListStyle`], such as `List_lower-alpha`.
fn list_style_name(style: ListStyle) -> String {
    format!("List_{}", style.name())
}

fn level_properties(level: usize) -> String {
    let margin = office::number(level as f64 * 36.0);
    format!(
        "<style:list-level-properties text:list-level-position-and-space-mode=\"label-alignment\">\
         <style:list-level-label-alignment text:label-followed-by=\"listtab\" text:list-tab-stop-position=\"{margin}pt\" \
         fo:text-indent=\"-18pt\" fo:margin-left=\"{margin}pt\"/></style:list-level-properties>"
    )
}

/// The common styles, matching the page Docs writes and the Word file.
const STYLES: &str = "<office:styles>\
<style:default-style style:family=\"paragraph\"><style:paragraph-properties fo:margin-top=\"0pt\" fo:margin-bottom=\"6pt\" fo:line-height=\"115%\"/>\
<style:text-properties style:font-name=\"Arimo\" fo:font-size=\"11pt\" fo:color=\"#1f1f1f\"/></style:default-style>\
<style:style style:name=\"Standard\" style:family=\"paragraph\" style:class=\"text\"/>\
<style:style style:name=\"Title\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:next-style-name=\"Standard\" style:class=\"chapter\">\
<style:paragraph-properties fo:margin-top=\"0pt\" fo:margin-bottom=\"3pt\" fo:keep-with-next=\"always\"/><style:text-properties fo:font-size=\"26pt\"/></style:style>\
<style:style style:name=\"Subtitle\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:next-style-name=\"Standard\" style:class=\"chapter\">\
<style:paragraph-properties fo:margin-top=\"0pt\" fo:margin-bottom=\"16pt\" fo:keep-with-next=\"always\"/><style:text-properties fo:font-size=\"15pt\" fo:color=\"#666666\"/></style:style>\
<style:style style:name=\"Heading_20_1\" style:display-name=\"Heading 1\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:next-style-name=\"Standard\" style:default-outline-level=\"1\" style:class=\"text\">\
<style:paragraph-properties fo:margin-top=\"20pt\" fo:margin-bottom=\"6pt\" fo:keep-with-next=\"always\"/><style:text-properties fo:font-size=\"20pt\"/></style:style>\
<style:style style:name=\"Heading_20_2\" style:display-name=\"Heading 2\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:next-style-name=\"Standard\" style:default-outline-level=\"2\" style:class=\"text\">\
<style:paragraph-properties fo:margin-top=\"18pt\" fo:margin-bottom=\"6pt\" fo:keep-with-next=\"always\"/><style:text-properties fo:font-size=\"16pt\"/></style:style>\
<style:style style:name=\"Heading_20_3\" style:display-name=\"Heading 3\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:next-style-name=\"Standard\" style:default-outline-level=\"3\" style:class=\"text\">\
<style:paragraph-properties fo:margin-top=\"16pt\" fo:margin-bottom=\"4pt\" fo:keep-with-next=\"always\"/><style:text-properties fo:font-size=\"14pt\" fo:color=\"#434343\"/></style:style>\
<style:style style:name=\"Heading_20_4\" style:display-name=\"Heading 4\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:next-style-name=\"Standard\" style:default-outline-level=\"4\" style:class=\"text\">\
<style:paragraph-properties fo:margin-top=\"14pt\" fo:margin-bottom=\"4pt\" fo:keep-with-next=\"always\"/><style:text-properties fo:font-size=\"12pt\" fo:color=\"#666666\"/></style:style>\
<style:style style:name=\"Heading_20_5\" style:display-name=\"Heading 5\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:next-style-name=\"Standard\" style:default-outline-level=\"5\" style:class=\"text\">\
<style:paragraph-properties fo:margin-top=\"12pt\" fo:margin-bottom=\"4pt\" fo:keep-with-next=\"always\"/><style:text-properties fo:color=\"#666666\"/></style:style>\
<style:style style:name=\"Heading_20_6\" style:display-name=\"Heading 6\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:next-style-name=\"Standard\" style:default-outline-level=\"6\" style:class=\"text\">\
<style:paragraph-properties fo:margin-top=\"12pt\" fo:margin-bottom=\"4pt\" fo:keep-with-next=\"always\"/><style:text-properties fo:color=\"#666666\" fo:font-style=\"italic\"/></style:style>\
<style:style style:name=\"Quotations\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:class=\"html\">\
<style:paragraph-properties fo:padding-left=\"8pt\" fo:border-left=\"2.25pt solid #dadce0\" fo:border-right=\"none\" fo:border-top=\"none\" fo:border-bottom=\"none\"/>\
<style:text-properties fo:color=\"#444746\"/></style:style>\
<style:style style:name=\"Preformatted_20_Text\" style:display-name=\"Preformatted Text\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:class=\"html\">\
<style:paragraph-properties fo:margin-top=\"0pt\" fo:margin-bottom=\"0pt\" fo:line-height=\"100%\" fo:background-color=\"#f1f3f4\"/>\
<style:text-properties style:font-name=\"Cousine\" fo:font-size=\"10pt\"/></style:style>\
<style:style style:name=\"Rule\" style:family=\"paragraph\" style:parent-style-name=\"Standard\">\
<style:paragraph-properties fo:padding-bottom=\"2pt\" fo:border-bottom=\"0.75pt solid #c4c7c5\" fo:border-top=\"none\" fo:border-left=\"none\" fo:border-right=\"none\"/></style:style>\
<style:style style:name=\"Table_20_Contents\" style:display-name=\"Table Contents\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:class=\"extra\">\
<style:paragraph-properties fo:margin-bottom=\"0pt\"/></style:style>\
<style:style style:name=\"Table_20_Heading\" style:display-name=\"Table Heading\" style:family=\"paragraph\" style:parent-style-name=\"Table_20_Contents\" style:class=\"extra\">\
<style:text-properties fo:font-weight=\"bold\"/></style:style>\
<style:style style:name=\"Contents_20_1\" style:display-name=\"Contents 1\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:class=\"index\">\
<style:paragraph-properties fo:margin-left=\"0pt\" fo:margin-bottom=\"4pt\"/></style:style>\
<style:style style:name=\"Contents_20_2\" style:display-name=\"Contents 2\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:class=\"index\">\
<style:paragraph-properties fo:margin-left=\"18pt\" fo:margin-bottom=\"4pt\"/></style:style>\
<style:style style:name=\"Contents_20_3\" style:display-name=\"Contents 3\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:class=\"index\">\
<style:paragraph-properties fo:margin-left=\"36pt\" fo:margin-bottom=\"4pt\"/></style:style>\
<style:style style:name=\"Contents_20_4\" style:display-name=\"Contents 4\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:class=\"index\">\
<style:paragraph-properties fo:margin-left=\"54pt\" fo:margin-bottom=\"4pt\"/></style:style>\
<style:style style:name=\"Contents_20_5\" style:display-name=\"Contents 5\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:class=\"index\">\
<style:paragraph-properties fo:margin-left=\"72pt\" fo:margin-bottom=\"4pt\"/></style:style>\
<style:style style:name=\"Contents_20_6\" style:display-name=\"Contents 6\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:class=\"index\">\
<style:paragraph-properties fo:margin-left=\"90pt\" fo:margin-bottom=\"4pt\"/></style:style>\
<style:style style:name=\"Internet_20_link\" style:display-name=\"Internet link\" style:family=\"text\">\
<style:text-properties fo:color=\"#1155cc\" style:text-underline-style=\"solid\" style:text-underline-width=\"auto\" style:text-underline-color=\"font-color\"/></style:style>\
</office:styles>";

/// The bookmark a table of contents entry links to, as LibreOffice names
/// them.
fn toc_name(entry: usize) -> String {
    format!("__RefHeading___Toc{}", entry + 1)
}

const MANIFEST: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<manifest:manifest xmlns:manifest=\"urn:oasis:names:tc:opendocument:xmlns:manifest:1.0\" manifest:version=\"1.3\">\
<manifest:file-entry manifest:full-path=\"/\" manifest:version=\"1.3\" manifest:media-type=\"application/vnd.oasis.opendocument.text\"/>\
<manifest:file-entry manifest:full-path=\"content.xml\" manifest:media-type=\"text/xml\"/>\
<manifest:file-entry manifest:full-path=\"styles.xml\" manifest:media-type=\"text/xml\"/>\
<manifest:file-entry manifest:full-path=\"meta.xml\" manifest:media-type=\"text/xml\"/>\
</manifest:manifest>";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PageSize;
    use std::io::Read;

    fn part(file: &[u8], path: &str) -> String {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(file)).unwrap();
        let mut text = String::new();
        zip.by_name(path)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        text
    }

    fn text(text: &str) -> Inline {
        Inline::Text(text.to_owned(), Marks::default())
    }

    #[test]
    fn writes_contents_as_opendocuments() {
        let blocks = crate::html::tests::with_contents();
        let file = write(
            &blocks,
            &Parts::default(),
            None,
            "Contents",
            &Pictures::new(),
            &[1, 1, 2],
        );
        let content = part(&file, "content.xml");
        assert!(
            content.contains("<text:table-of-content text:name=\"Table of Contents1\""),
            "{content}"
        );
        assert!(
            content.contains(
                "<text:p text:style-name=\"TocDotted3\"><text:a xlink:type=\"simple\" xlink:href=\"#__RefHeading___Toc3\">\
                 Café &amp; tea!<text:tab/>2</text:a></text:p>"
            ),
            "{content}"
        );
        assert!(
            content.contains("<text:bookmark-start text:name=\"__RefHeading___Toc1\"/>Plan"),
            "{content}"
        );
        assert!(content.contains("style:name=\"TocDotted3\""), "{content}");
    }

    #[test]
    fn writes_footnotes() {
        let blocks = crate::html::tests::noted();
        let file = write(
            &blocks,
            &Parts::default(),
            None,
            "Notes",
            &Pictures::new(),
            &[],
        );
        let content = part(&file, "content.xml");
        assert!(
            content.contains(
                "Claim<text:note text:id=\"ftn1\" text:note-class=\"footnote\"><text:note-citation>1</text:note-citation>\
                 <text:note-body><text:p text:style-name=\"Footnote\">See "
            ),
            "{content}"
        );
        assert!(
            content.contains("<text:note-citation>2</text:note-citation><text:note-body><text:p text:style-name=\"Footnote\">Two</text:p>"),
            "{content}"
        );
    }

    #[test]
    fn writes_headers_footers_and_page_numbers() {
        let parts = crate::html::tests::parts();
        let blocks = vec![Block::Paragraph(Para::default(), Vec::new())];
        let file = write(&blocks, &parts, None, "Parts", &Pictures::new(), &[]);
        let styles = crate::office::tests::unzip(&file, "styles.xml");
        assert!(styles.contains("<style:header><text:p"), "{styles}");
        assert!(
            styles.contains("<text:page-number text:select-page=\"current\">1</text:page-number>"),
            "{styles}"
        );
        assert!(
            styles.contains("<text:page-count>1</text:page-count>"),
            "{styles}"
        );
        assert!(
            styles.contains("<style:header-first><text:p/></style:header-first>"),
            "{styles}"
        );
        assert!(styles.contains("<style:footer-first><text:p"), "{styles}");
        // The header's half inch from the top, and the text an inch down.
        assert!(styles.contains("fo:margin-top=\"36pt\""), "{styles}");
        assert!(styles.contains("fo:min-height=\"36pt\""), "{styles}");
        // Its small text's style is in the styles file too.
        assert!(styles.contains("fo:font-size=\"9pt\""), "{styles}");
    }

    #[test]
    fn writes_an_opendocument_file_on_the_documents_paper() {
        let bold = Marks {
            bold: true,
            ..Marks::default()
        };
        let blocks = vec![
            Block::Heading(2, Para::default(), vec![text("Plan")]),
            Block::Paragraph(
                Para::default(),
                vec![
                    text(" two  spaces"),
                    Inline::Text("bold".to_owned(), bold),
                    Inline::Text(
                        "styled".to_owned(),
                        Marks {
                            color: Some(Color([0xcc, 0, 0])),
                            highlight: Some(Color([0xff, 0xf2, 0xcc])),
                            size: Some(21),
                            font: Some("Open Sans".into()),
                            script: Some(Script::Super),
                            ..Marks::default()
                        },
                    ),
                ],
            ),
            Block::Numbers(
                3,
                None,
                vec![vec![Block::Paragraph(Para::default(), vec![text("Third")])]],
            ),
            Block::Checks(vec![(
                true,
                vec![Block::Paragraph(Para::default(), vec![text("Done")])],
            )]),
        ];
        let setup = PageSetup {
            size: PageSize::Letter,
            landscape: false,
            margins: [72.0, 36.0, 72.0, 36.0],
        };
        let file = write(
            &blocks,
            &crate::model::Parts::default(),
            Some(&setup),
            "Plan",
            &Pictures::new(),
            &[],
        );
        // The mimetype comes first, stored as is, as OpenDocument asks.
        assert_eq!(&file[30..38], b"mimetype");
        assert_eq!(&file[38..38 + MIME_TYPE.len()], MIME_TYPE.as_bytes());
        let content = part(&file, "content.xml");
        assert!(
            content.contains(
                "<text:h text:style-name=\"Heading_20_2\" text:outline-level=\"2\">Plan</text:h>"
            ),
            "{content}"
        );
        assert!(
            content.contains("<text:s/>two <text:s/>spaces<text:span text:style-name=\"Span"),
            "{content}"
        );
        assert!(
            content.contains("<text:list-item text:start-value=\"3\">"),
            "{content}"
        );
        assert!(
            content.contains("<text:list text:style-name=\"Checked\">"),
            "{content}"
        );
        assert!(content.contains("text-line-through-style"), "{content}");
        assert!(
            content.contains(
                "fo:font-family=\"&apos;Open Sans&apos;\" fo:background-color=\"#fff2cc\" fo:color=\"#cc0000\" fo:font-size=\"10.5pt\" style:text-position=\"super 58%\""
            ),
            "{content}"
        );
        let styles = part(&file, "styles.xml");
        assert!(
            styles.contains("fo:page-width=\"612pt\" fo:page-height=\"792pt\""),
            "{styles}"
        );
        assert!(styles.contains("fo:margin-right=\"36pt\""), "{styles}");
        assert!(part(&file, "meta.xml").contains("<dc:title>Plan</dc:title>"));

        // Pictures go in the file, once each, sized to fit the page.
        let picture = Picture::read(crate::picture::tests::png(400, 300)).unwrap();
        let image = Block::Image(Image {
            hash: picture.hash.clone(),
            alt: "Dot".into(),
            width: Some(200),
        });
        let pictures: Pictures = [(picture.hash.clone(), picture.clone())]
            .into_iter()
            .collect();
        let file = write(
            &[image],
            &crate::model::Parts::default(),
            Some(&setup),
            "Pictures",
            &pictures,
            &[],
        );
        let content = part(&file, "content.xml");
        let path = format!("Pictures/{}.png", picture.hash);
        assert!(
            content.contains(&format!(
                "svg:width=\"150pt\" svg:height=\"112.5pt\"><draw:image xlink:href=\"{path}\""
            )),
            "{content}"
        );
        assert!(content.contains("<svg:desc>Dot</svg:desc>"), "{content}");
        assert!(part(&file, "META-INF/manifest.xml").contains(&path));
    }

    #[test]
    fn writes_cell_colors() {
        let file = write(
            &crate::html::tests::colored_cells(),
            &crate::model::Parts::default(),
            None,
            "Cells",
            &Pictures::new(),
            &[],
        );
        let content = part(&file, "content.xml");
        assert!(content.contains(
            "<style:style style:name=\"HeadingCell1\" style:family=\"table-cell\">\
             <style:table-cell-properties fo:background-color=\"#fce5cd\""
        ));
        assert!(content.contains("<table:table-cell table:style-name=\"HeadingCell1\""));
        assert!(content.contains("<table:table-cell table:style-name=\"HeadingCell\""));
        assert!(content.contains("<table:table-cell table:style-name=\"Cell2\""));
    }

    #[test]
    fn writes_borders_padding_row_heights_and_lines() {
        let file = write(
            &crate::html::tests::google_layout(),
            &crate::model::Parts::default(),
            None,
            "Resume",
            &Pictures::new(),
            &[],
        );
        let content = part(&file, "content.xml");
        assert!(content.contains(
            "<style:table-cell-properties fo:padding-top=\"3.6pt\" fo:padding-right=\"3.6pt\" \
             fo:padding-bottom=\"3.6pt\" fo:padding-left=\"3.6pt\" fo:border-top=\"none\""
        ));
        assert!(content.contains("fo:border-top=\"1pt solid #ff0000\" fo:border-right=\"none\""));
        assert!(content.contains("<style:table-row-properties style:min-row-height=\"120pt\"/>"));
        assert!(content.contains("<table:table-row table:style-name=\"Table1.R0\">"));
        assert!(
            content.contains(
                "<style:paragraph-properties fo:border-bottom=\"2.25pt solid #000000\"/>"
            )
        );
        assert!(content.contains("<text:p text:style-name=\"Rule1\"/>"));
    }

    #[test]
    fn writes_paragraph_layout_list_styles_and_page_breaks() {
        let blocks = crate::html::tests::laid_out();
        let file = write(
            &blocks,
            &crate::model::Parts::default(),
            None,
            "Layout",
            &Pictures::new(),
            &[],
        );
        let content = part(&file, "content.xml");
        assert!(content.contains(
            "style:parent-style-name=\"Standard\"><style:paragraph-properties fo:text-align=\"justify\" \
             fo:margin-left=\"36pt\" fo:margin-right=\"18pt\" fo:text-indent=\"-18pt\" fo:line-height=\"150%\" \
             fo:margin-top=\"12pt\" fo:margin-bottom=\"0pt\"/>"
        ));
        assert!(content.contains("<text:p text:style-name=\"PageBreak\"/>"));
        assert!(content.contains("<text:list text:style-name=\"List_upper-roman\">"));
        assert!(content.contains("style:num-format=\"I\""));
        // The title is a style of its own, centered by one of the document's.
        assert!(content.contains("style:parent-style-name=\"Title\"><style:paragraph-properties fo:text-align=\"center\"/>"));
        assert!(part(&file, "styles.xml").contains("style:name=\"Subtitle\""));
    }

    #[test]
    fn writes_weights_as_numbers() {
        let marks = |weight: Option<u16>, bold: bool| Marks {
            font: Some("Raleway".into()),
            weight,
            bold,
            ..Marks::default()
        };
        let blocks = vec![Block::Paragraph(
            Para::default(),
            vec![
                Inline::Text("light".into(), marks(Some(300), false)),
                Inline::Text("light, bold".into(), marks(Some(300), true)),
                Inline::Text("black".into(), marks(Some(900), false)),
            ],
        )];
        let file = write(
            &blocks,
            &Parts::default(),
            None,
            "Fonts",
            &Pictures::new(),
            &[],
        );
        let content = part(&file, "content.xml");
        assert!(content.contains("fo:font-weight=\"300\""), "{content}");
        assert!(content.contains("fo:font-weight=\"900\""), "{content}");
        assert!(
            content.contains("style:font-name=\"Arimo\"")
                || part(&file, "styles.xml").contains("style:font-name=\"Arimo\"")
        );
    }
}
