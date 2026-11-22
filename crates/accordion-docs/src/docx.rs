//! A document as a Word file (`.docx`), for **Download as** (decision 30).
//!
//! It keeps everything the editor has: headings, marks, links, lists to
//! nine levels, checklists (as ☐ and ☑ bullets, with done items struck
//! through as on the page), quotes, code, rules, and tables with merged
//! cells and column widths. The page setup becomes the section's paper and
//! margins. Text is Arial 11 point, as in Google Docs. Headers and footers,
//! the first page's own too, are Word's, with page numbers and counts as
//! Word's fields. Footnotes are Word's, numbered by Word. A table of
//! contents is Word's TOC field, with its entries and page numbers already
//! in it, so it shows as it does in Docs and Word can update it.

use std::fmt::Write as _;

use crate::model::{
    self, Align, Block, ContentsStyle, Field, Image, Inline, Kind, ListStyle, Marks, PageSetup,
    Para, Parts, Row, Script,
};
use crate::office::{self, Grid, Part, Slot, escape, escaped};
use crate::picture::{Picture, Pictures};

/// `blocks` on `setup`'s page as a Word file, with `pictures` in it and
/// `title` in its properties. `pages` is the page each entry of a table of
/// contents is on, as the editor lays the pages out, or empty if unknown.
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
        out: String::new(),
        links: Vec::new(),
        numbered: Vec::new(),
        pictures,
        media: Vec::new(),
        drawings: 0,
        notes: Vec::new(),
        anchors: Vec::new(),
        headings: 0,
        contents: Vec::new(),
        pages: Vec::new(),
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
    let notes = std::mem::take(&mut writer.notes);
    // Each header and footer is a part of its own, with Word's name for
    // where it goes.
    let mut margins: Vec<(String, &str, &str, String)> = Vec::new();
    for (name, blocks) in parts.each() {
        if blocks.is_empty() {
            continue;
        }
        writer.blocks(blocks, &Context::top(room));
        let content = std::mem::take(&mut writer.out);
        let (tag, kind) = match name {
            "header" => ("hdr", "default"),
            "footer" => ("ftr", "default"),
            "first_header" => ("hdr", "first"),
            _ => ("ftr", "first"),
        };
        let xml = format!(
            "{XML}<w:{tag} xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
             xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
             xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" \
             xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
             xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">{content}</w:{tag}>"
        );
        margins.push((name.replace('_', "-"), tag, kind, xml));
    }

    let mut document = String::from(XML);
    document.push_str(
        "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
         xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
         xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" \
         xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
         xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><w:body>",
    );
    document.push_str(&body);
    let (width, height) = page.points();
    let [top, right, bottom, left] = page.margins.map(twips);
    let _ = write!(
        document,
        "<w:sectPr>{}<w:pgSz w:w=\"{}\" w:h=\"{}\"{}/>\
         <w:pgMar w:top=\"{top}\" w:right=\"{right}\" w:bottom=\"{bottom}\" w:left=\"{left}\" \
         w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/>{}</w:sectPr></w:body></w:document>",
        margins
            .iter()
            .map(|(name, tag, kind, _)| format!(
                "<w:{}Reference w:type=\"{kind}\" r:id=\"rId-{name}\"/>",
                if *tag == "hdr" { "header" } else { "footer" }
            ))
            .collect::<String>(),
        twips(width),
        twips(height),
        if page.landscape {
            " w:orient=\"landscape\""
        } else {
            ""
        },
        if parts.different_first {
            "<w:titlePg/>"
        } else {
            ""
        },
    );

    let mut relations = String::from(XML);
    relations.push_str(
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
         <Relationship Id=\"rIdStyles\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>\
         <Relationship Id=\"rIdNumbering\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering\" Target=\"numbering.xml\"/>",
    );
    for (index, href) in writer.links.iter().enumerate() {
        let _ = write!(
            relations,
            "<Relationship Id=\"rIdLink{index}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink\" \
             Target=\"{}\" TargetMode=\"External\"/>",
            escaped(href)
        );
    }
    for (index, picture) in writer.media.iter().enumerate() {
        let _ = write!(
            relations,
            "<Relationship Id=\"rIdPicture{index}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" \
             Target=\"media/picture{index}.{}\"/>",
            picture.extension()
        );
    }
    relations.push_str("</Relationships>");
    // Headers and footers can hold links and pictures too, by the same IDs.
    let part_relations = relations.clone();
    let mut extra = String::new();
    for (name, tag, _, _) in &margins {
        let kind = if *tag == "hdr" { "header" } else { "footer" };
        let _ = write!(
            extra,
            "<Relationship Id=\"rId-{name}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}\" \
             Target=\"{name}.xml\"/>"
        );
    }
    if !notes.is_empty() {
        extra.push_str(
            "<Relationship Id=\"rIdFootnotes\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes\" \
             Target=\"footnotes.xml\"/>",
        );
    }
    relations = relations.replace("</Relationships>", &format!("{extra}</Relationships>"));
    // Word's own separator line and its continuation come first, by the
    // numbers Word gives them.
    let footnotes = format!(
        "{XML}<w:footnotes xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
         xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
         <w:footnote w:type=\"separator\" w:id=\"-1\"><w:p><w:pPr><w:spacing w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr>\
         <w:r><w:separator/></w:r></w:p></w:footnote>\
         <w:footnote w:type=\"continuationSeparator\" w:id=\"0\"><w:p><w:pPr><w:spacing w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr>\
         <w:r><w:continuationSeparator/></w:r></w:p></w:footnote>{}</w:footnotes>",
        notes.concat()
    );

    let core = format!(
        "{XML}<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" \
         xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title>{}</dc:title></cp:coreProperties>",
        escaped(title)
    );
    let numbering = numbering(&writer.numbered);
    let mut content_types = CONTENT_TYPES.to_owned();
    let mut kinds: Vec<&Picture> = Vec::new();
    for picture in &writer.media {
        if !kinds
            .iter()
            .any(|kind| kind.extension() == picture.extension())
        {
            kinds.push(picture);
        }
    }
    let defaults: String = kinds
        .iter()
        .map(|picture| {
            format!(
                "<Default Extension=\"{}\" ContentType=\"{}\"/>",
                picture.extension(),
                picture.content_type
            )
        })
        .collect();
    content_types = content_types.replace(
        "<Default Extension=\"xml\"",
        &format!("{defaults}<Default Extension=\"xml\""),
    );
    let overrides: String = margins
        .iter()
        .map(|(name, tag, _, _)| {
            format!(
                "<Override PartName=\"/word/{name}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.{}+xml\"/>",
                if *tag == "hdr" { "header" } else { "footer" }
            )
        })
        .collect();
    content_types = content_types.replace("</Types>", &format!("{overrides}</Types>"));
    if !notes.is_empty() {
        content_types = content_types.replace(
            "</Types>",
            "<Override PartName=\"/word/footnotes.xml\" \
             ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml\"/></Types>",
        );
    }
    let margin_paths: Vec<(String, String)> = margins
        .iter()
        .map(|(name, _, _, _)| {
            (
                format!("word/{name}.xml"),
                format!("word/_rels/{name}.xml.rels"),
            )
        })
        .collect();
    let media_paths: Vec<String> = writer
        .media
        .iter()
        .enumerate()
        .map(|(index, picture)| format!("word/media/picture{index}.{}", picture.extension()))
        .collect();
    let mut parts = vec![
        Part {
            path: "[Content_Types].xml",
            contents: content_types.as_bytes(),
            stored: false,
        },
        Part {
            path: "_rels/.rels",
            contents: PACKAGE_RELATIONS.as_bytes(),
            stored: false,
        },
        Part {
            path: "docProps/core.xml",
            contents: core.as_bytes(),
            stored: false,
        },
        Part {
            path: "word/document.xml",
            contents: document.as_bytes(),
            stored: false,
        },
        Part {
            path: "word/_rels/document.xml.rels",
            contents: relations.as_bytes(),
            stored: false,
        },
        Part {
            path: "word/styles.xml",
            contents: STYLES.as_bytes(),
            stored: false,
        },
        Part {
            path: "word/numbering.xml",
            contents: numbering.as_bytes(),
            stored: false,
        },
    ];
    for ((path, relations_path), (_, _, _, xml)) in margin_paths.iter().zip(&margins) {
        parts.push(Part {
            path,
            contents: xml.as_bytes(),
            stored: false,
        });
        parts.push(Part {
            path: relations_path,
            contents: part_relations.as_bytes(),
            stored: false,
        });
    }
    if !notes.is_empty() {
        parts.push(Part {
            path: "word/footnotes.xml",
            contents: footnotes.as_bytes(),
            stored: false,
        });
        parts.push(Part {
            path: "word/_rels/footnotes.xml.rels",
            contents: part_relations.as_bytes(),
            stored: false,
        });
    }
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

const XML: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";

/// Points in twentieths of a point, Word's unit for lengths.
fn twips(points: f64) -> i64 {
    (points * 20.0).round() as i64
}

/// Word's list levels go from 0 to 8.
const DEEPEST: usize = 8;

/// The numbering definitions: bullets, ☐ and ☑ for checklists, and numbers.
const BULLETS: usize = 1;
const UNCHECKED: usize = 2;
const CHECKED: usize = 3;
/// Each numbered list gets a numbering of its own from here on, so it
/// starts at its own number.
const FIRST_NUMBERED: usize = 4;
/// The numbering, and its definition, for each bulleted [`ListStyle`], by
/// its place in [`ListStyle::ALL`]. Numbered styles' definitions are here
/// too, used by each numbered list's own numbering.
const STYLED: usize = 1000;

fn styled(style: ListStyle) -> usize {
    STYLED
        + ListStyle::ALL
            .iter()
            .position(|known| *known == style)
            .unwrap_or_default()
}

/// Where blocks are being written, which decides their paragraphs' look.
#[derive(Clone)]
struct Context {
    /// How many quotes deep.
    quotes: usize,
    /// How many lists deep.
    lists: usize,
    /// The numbering and level for the next paragraph, the first of a list
    /// item. Later paragraphs in the item have none, lined up with its text.
    item: Option<(usize, usize)>,
    /// Struck through and gray: a done checklist item's text.
    done: bool,
    /// In a heading cell: bold.
    header: bool,
    /// The room for text across, in points, for tables.
    room: f64,
}

impl Context {
    fn top(room: f64) -> Self {
        Self {
            quotes: 0,
            lists: 0,
            item: None,
            done: false,
            header: false,
            room,
        }
    }

    /// How far paragraphs here start from the margin, in twips.
    fn indent(&self) -> i64 {
        (self.quotes * 360 + self.lists.min(DEEPEST + 1) * 720) as i64
    }
}

struct Writer<'a> {
    out: String,
    /// Each link's address, by its place: `rIdLink{index}`.
    links: Vec<String>,
    /// Each numbered list's level, first number, and style, by its place
    /// after `FIRST_NUMBERED`.
    numbered: Vec<(usize, u32, Option<ListStyle>)>,
    pictures: &'a Pictures,
    /// Each picture in the file, by its place: `rIdPicture{index}`.
    media: Vec<Picture>,
    /// How many pictures are drawn, for their IDs.
    drawings: usize,
    /// Each footnote, as Word's `<w:footnote>`, numbered from 1.
    notes: Vec<String>,
    /// Each heading's anchor, while the document has a table of contents,
    /// and how many headings are written.
    anchors: Vec<Option<String>>,
    headings: usize,
    /// The table of contents' entries, and each one's page, if known.
    contents: Vec<model::Entry>,
    pages: Vec<u32>,
}

impl Writer<'_> {
    fn blocks(&mut self, blocks: &[Block], context: &Context) {
        let mut context = context.clone();
        let mut after_table = false;
        for block in blocks {
            // Two tables in a row would join into one in Word.
            if after_table && matches!(block, Block::Table(_)) {
                self.paragraph(&context, None, &Para::default(), &[]);
            }
            self.block(block, &context);
            after_table = matches!(block, Block::Table(_));
            // Only an item's first paragraph has its bullet or number.
            context.item = None;
            context.done = false;
        }
    }

    fn block(&mut self, block: &Block, context: &Context) {
        match block {
            Block::Paragraph(para, inlines) => {
                let style = match para.kind {
                    Some(Kind::Title) => Some("Title"),
                    Some(Kind::Subtitle) => Some("Subtitle"),
                    None => None,
                };
                self.paragraph(context, style, para, inlines);
            }
            Block::Heading(level, para, inlines) => {
                let style = format!("Heading{}", (*level).clamp(1, 6));
                let start = self.out.len();
                self.paragraph(context, Some(&style), para, inlines);
                self.bookmark(start);
            }
            Block::Contents(style) => self.contents(*style, context),
            Block::PageBreak => self
                .out
                .push_str("<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>"),
            Block::Quote(blocks) => {
                let mut inner = context.clone();
                inner.quotes += 1;
                self.blocks(blocks, &inner);
            }
            Block::Code(text) => {
                let lines: Vec<&str> = text
                    .strip_suffix('\n')
                    .unwrap_or(text)
                    .split('\n')
                    .collect();
                for line in lines {
                    let code = Inline::Text(line.to_owned(), Marks::default());
                    self.paragraph(
                        context,
                        Some("Code"),
                        &Para::default(),
                        std::slice::from_ref(&code),
                    );
                }
            }
            Block::Rule(rule) => {
                let size = rule.thickness.map_or(6, eighths);
                let color = rule
                    .color
                    .map_or("C4C7C5".to_owned(), |color| color.hex_upper());
                let _ = write!(
                    self.out,
                    "<w:p><w:pPr><w:pBdr><w:bottom w:val=\"single\" w:sz=\"{size}\" w:space=\"1\" w:color=\"{color}\"/></w:pBdr>"
                );
                self.indent(context, false, &Para::default());
                self.out.push_str("</w:pPr></w:p>");
            }
            Block::Bullets(style, items) => {
                let numbering = style.map_or(BULLETS, styled);
                for item in items {
                    self.item(item, context, numbering, false);
                }
            }
            Block::Numbers(start, style, items) => {
                let level = context.lists.min(DEEPEST);
                self.numbered.push((level, *start, *style));
                let numbering = FIRST_NUMBERED + self.numbered.len() - 1;
                for item in items {
                    self.item(item, context, numbering, false);
                }
            }
            Block::Checks(items) => {
                for (checked, item) in items {
                    let numbering = if *checked { CHECKED } else { UNCHECKED };
                    self.item(item, context, numbering, *checked);
                }
            }
            Block::Table(rows) => self.table(rows, context),
            Block::Image(image) => self.image(image, context),
        }
    }

    /// A picture in a paragraph of its own, as large as it's shown on the
    /// page. One whose contents are missing is left out.
    fn image(&mut self, image: &Image, context: &Context) {
        let pictures = self.pictures;
        let Some(picture) = pictures.get(&image.hash) else {
            return;
        };
        let index = match self.media.iter().position(|kept| kept.hash == picture.hash) {
            Some(index) => index,
            None => {
                self.media.push(picture.clone());
                self.media.len() - 1
            }
        };
        self.drawings += 1;
        let id = self.drawings;
        let room = context.room - context.indent() as f64 / 20.0;
        let (width, height) = office::picture_size(picture, image.width, room);
        // English Metric Units: 12,700 to the point.
        let (cx, cy) = (
            (width * 12700.0).round() as i64,
            (height * 12700.0).round() as i64,
        );
        let alt = escaped(&image.alt);
        self.out.push_str("<w:p><w:pPr>");
        if context.quotes > 0 {
            self.out.push_str("<w:pStyle w:val=\"Quote\"/>");
        }
        self.indent(context, false, &Para::default());
        let _ = write!(
            self.out,
            "</w:pPr><w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\">\
             <wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:docPr id=\"{id}\" name=\"Picture {id}\" descr=\"{alt}\"/>\
             <a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
             <pic:pic><pic:nvPicPr><pic:cNvPr id=\"{id}\" name=\"Picture {id}\" descr=\"{alt}\"/><pic:cNvPicPr/></pic:nvPicPr>\
             <pic:blipFill><a:blip r:embed=\"rIdPicture{index}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
             <pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm>\
             <a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr></pic:pic>\
             </a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"
        );
    }

    fn item(&mut self, blocks: &[Block], context: &Context, numbering: usize, done: bool) {
        let mut inner = context.clone();
        inner.item = Some((numbering, context.lists.min(DEEPEST)));
        inner.lists += 1;
        inner.done = done;
        if blocks.is_empty() {
            self.paragraph(&inner, None, &Para::default(), &[]);
        } else {
            self.blocks(blocks, &inner);
        }
    }

    /// Writes the indent for a paragraph inside quotes or lists, and its
    /// own. A list item's first paragraph hangs its bullet or number in the
    /// indent, unless its own first line says otherwise.
    fn indent(&mut self, context: &Context, hanging: bool, para: &Para) {
        let left = context.indent() + i64::from(para.indent);
        let right = i64::from(para.indent_right);
        let first = match (para.first_line, hanging) {
            (0, true) => -360,
            (first, _) => i64::from(first),
        };
        if left == 0 && right == 0 && first == 0 {
            return;
        }
        let _ = write!(self.out, "<w:ind w:left=\"{left}\" w:right=\"{right}\"");
        match first {
            0 => {}
            first if first < 0 => {
                let _ = write!(self.out, " w:hanging=\"{}\"", -first);
            }
            first => {
                let _ = write!(self.out, " w:firstLine=\"{first}\"");
            }
        }
        self.out.push_str("/>");
    }

    /// Marks the heading written from `start` as the place its table of
    /// contents entry links to, if it has one.
    fn bookmark(&mut self, start: usize) {
        let index = self.headings;
        self.headings += 1;
        let Some(anchor) = self.anchors.get(index).cloned().flatten() else {
            return;
        };
        let Some(entry) = self
            .contents
            .iter()
            .position(|entry| entry.anchor == anchor)
        else {
            return;
        };
        let Some(after) = self.out[start..].find("</w:pPr>") else {
            return;
        };
        let at = start + after + "</w:pPr>".len();
        self.out.insert_str(
            at,
            &format!(
                "<w:bookmarkStart w:id=\"{entry}\" w:name=\"{}\"/>",
                toc_name(entry)
            ),
        );
        if self.out.ends_with("</w:p>") {
            let end = self.out.len() - "</w:p>".len();
            self.out
                .insert_str(end, &format!("<w:bookmarkEnd w:id=\"{entry}\"/>"));
        }
    }

    /// A table of contents: Word's TOC field around a paragraph for each
    /// heading, each a link to its heading's bookmark with a PAGEREF field
    /// for its page, as Word writes one.
    fn contents(&mut self, style: ContentsStyle, context: &Context) {
        let leader = if style == ContentsStyle::Dotted {
            "dot"
        } else {
            "none"
        };
        let tab = (context.room * 20.0).round() as i64;
        let switches = if style.numbered() { "" } else { " \\n" };
        let begin = format!(
            "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText xml:space=\"preserve\"> TOC \\o \"1-6\" \\h \\z \\u{switches} </w:instrText></w:r>\
             <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>"
        );
        let end = "<w:r><w:fldChar w:fldCharType=\"end\"/></w:r>";
        self.out.push_str(
            "<w:sdt><w:sdtPr><w:docPartObj><w:docPartGallery w:val=\"Table of Contents\"/><w:docPartUnique/></w:docPartObj></w:sdtPr><w:sdtContent>",
        );
        let entries = std::mem::take(&mut self.contents);
        if entries.is_empty() {
            let _ = write!(self.out, "<w:p>{begin}{end}</w:p>");
        }
        for (index, entry) in entries.iter().enumerate() {
            let _ = write!(
                self.out,
                "<w:p><w:pPr><w:pStyle w:val=\"TOC{}\"/><w:tabs><w:tab w:val=\"right\" w:leader=\"{leader}\" w:pos=\"{tab}\"/></w:tabs></w:pPr>",
                entry.level
            );
            if index == 0 {
                self.out.push_str(&begin);
            }
            let name = toc_name(index);
            let _ = write!(
                self.out,
                "<w:hyperlink w:anchor=\"{name}\" w:history=\"1\"><w:r>"
            );
            if !style.numbered() {
                self.out
                    .push_str("<w:rPr><w:rStyle w:val=\"Hyperlink\"/></w:rPr>");
            }
            self.out.push_str("<w:t xml:space=\"preserve\">");
            escape(&mut self.out, &entry.text);
            self.out.push_str("</w:t></w:r>");
            if style.numbered() {
                let page = self
                    .pages
                    .get(index)
                    .map(u32::to_string)
                    .unwrap_or_default();
                let _ = write!(
                    self.out,
                    "<w:r><w:tab/></w:r><w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
                     <w:r><w:instrText xml:space=\"preserve\"> PAGEREF {name} \\h </w:instrText></w:r>\
                     <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>{page}</w:t></w:r>\
                     <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>"
                );
            }
            self.out.push_str("</w:hyperlink>");
            if index + 1 == entries.len() {
                self.out.push_str(end);
            }
            self.out.push_str("</w:p>");
        }
        self.contents = entries;
        self.out.push_str("</w:sdtContent></w:sdt>");
    }

    fn paragraph(
        &mut self,
        context: &Context,
        style: Option<&str>,
        para: &Para,
        inlines: &[Inline],
    ) {
        self.out.push_str("<w:p><w:pPr>");
        let style = style.or(if context.quotes > 0 {
            Some("Quote")
        } else {
            None
        });
        if let Some(style) = style {
            let _ = write!(self.out, "<w:pStyle w:val=\"{style}\"/>");
        }
        if let Some((numbering, level)) = context.item {
            let _ = write!(
                self.out,
                "<w:numPr><w:ilvl w:val=\"{level}\"/><w:numId w:val=\"{numbering}\"/></w:numPr>"
            );
        }
        // Paragraphs in a list sit close together, as on the page.
        let after = para
            .after
            .map(i64::from)
            .or((context.lists > 0).then_some(40));
        if para.before.is_some() || after.is_some() || para.line.is_some() {
            self.out.push_str("<w:spacing");
            if let Some(before) = para.before {
                let _ = write!(self.out, " w:before=\"{before}\"");
            }
            if let Some(after) = after {
                let _ = write!(self.out, " w:after=\"{after}\"");
            }
            if let Some(line) = para.line {
                // Lines in 240ths of single spacing.
                let _ = write!(
                    self.out,
                    " w:line=\"{}\" w:lineRule=\"auto\"",
                    u32::from(line) * 240 / 100
                );
            }
            self.out.push_str("/>");
        }
        self.indent(context, context.item.is_some(), para);
        match para.align {
            Align::Left => {}
            Align::Center => self.out.push_str("<w:jc w:val=\"center\"/>"),
            Align::Right => self.out.push_str("<w:jc w:val=\"right\"/>"),
            Align::Justify => self.out.push_str("<w:jc w:val=\"both\"/>"),
        }
        self.out.push_str("</w:pPr>");
        self.runs(inlines, context);
        self.out.push_str("</w:p>");
    }

    fn runs(&mut self, inlines: &[Inline], context: &Context) {
        let mut link: Option<&str> = None;
        for inline in inlines {
            let marks = match inline {
                Inline::Text(_, marks) => Some(marks),
                Inline::Break | Inline::Footnote(_) => None,
            };
            let href = marks.and_then(|marks| marks.link.as_deref());
            if link.is_some() && href != link {
                self.out.push_str("</w:hyperlink>");
                link = None;
            }
            if let (Some(href), None) = (href, link) {
                let index = self.links.len();
                self.links.push(href.to_owned());
                let _ = write!(self.out, "<w:hyperlink r:id=\"rIdLink{index}\">");
                link = Some(href);
            }
            match inline {
                Inline::Text(_, marks) if marks.field.is_some() => self.field(marks, context),
                Inline::Text(text, marks) => self.run(text, marks, context),
                Inline::Break => self.out.push_str("<w:r><w:br/></w:r>"),
                Inline::Footnote(text) => self.footnote(text, context),
            }
        }
        if link.is_some() {
            self.out.push_str("</w:hyperlink>");
        }
    }

    /// A footnote's number here, and its text in the footnotes part.
    fn footnote(&mut self, text: &[Inline], context: &Context) {
        let id = self.notes.len() + 1;
        let _ = write!(
            self.out,
            "<w:r><w:rPr><w:rStyle w:val=\"FootnoteReference\"/></w:rPr><w:footnoteReference w:id=\"{id}\"/></w:r>"
        );
        let body = std::mem::take(&mut self.out);
        let _ = write!(
            self.out,
            "<w:footnote w:id=\"{id}\"><w:p><w:pPr><w:pStyle w:val=\"FootnoteText\"/></w:pPr>\
             <w:r><w:rPr><w:rStyle w:val=\"FootnoteReference\"/></w:rPr><w:footnoteRef/></w:r>\
             <w:r><w:t xml:space=\"preserve\"> </w:t></w:r>"
        );
        self.runs(text, &Context::top(context.room));
        self.out.push_str("</w:p></w:footnote>");
        let note = std::mem::replace(&mut self.out, body);
        self.notes.push(note);
    }

    /// A page number or count, as Word's field, with a number in it until
    /// Word works out each page's.
    fn field(&mut self, marks: &Marks, context: &Context) {
        let (instruction, shown) = match marks.field {
            Some(Field::PageCount) => ("NUMPAGES", "1"),
            _ => ("PAGE", "1"),
        };
        let _ = write!(self.out, "<w:fldSimple w:instr=\" {instruction} \">");
        let plain = Marks {
            field: None,
            ..marks.clone()
        };
        self.run(shown, &plain, context);
        self.out.push_str("</w:fldSimple>");
    }

    fn run(&mut self, text: &str, marks: &Marks, context: &Context) {
        if text.is_empty() {
            return;
        }
        self.out.push_str("<w:r><w:rPr>");
        // Word wants these in the schema's order.
        if marks.link.is_some() {
            self.out.push_str("<w:rStyle w:val=\"Hyperlink\"/>");
        }
        if let Some(font) = &marks.font {
            let font = escaped(font);
            let _ = write!(
                self.out,
                "<w:rFonts w:ascii=\"{font}\" w:hAnsi=\"{font}\" w:eastAsia=\"{font}\" w:cs=\"{font}\"/>"
            );
        } else if marks.code {
            self.out.push_str(
                "<w:rFonts w:ascii=\"Courier New\" w:hAnsi=\"Courier New\" w:cs=\"Courier New\"/>",
            );
        }
        if marks.bold || context.header {
            self.out.push_str("<w:b/>");
        }
        if marks.italic {
            self.out.push_str("<w:i/>");
        }
        if marks.strike || context.done {
            self.out.push_str("<w:strike/>");
        }
        if context.done {
            self.out.push_str("<w:color w:val=\"5F6368\"/>");
        } else if let Some(color) = marks.color {
            let _ = write!(self.out, "<w:color w:val=\"{}\"/>", color.hex_upper());
        }
        if let Some(size) = marks.size {
            let _ = write!(
                self.out,
                "<w:sz w:val=\"{size}\"/><w:szCs w:val=\"{size}\"/>"
            );
        }
        if marks.underline {
            self.out.push_str("<w:u w:val=\"single\"/>");
        }
        // Word's own highlights come in 16 colors only, so any color is
        // shading, as Google Docs writes it.
        if let Some(color) = marks.highlight {
            let _ = write!(
                self.out,
                "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{}\"/>",
                color.hex_upper()
            );
        } else if marks.code {
            self.out
                .push_str("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"F1F3F4\"/>");
        }
        match marks.script {
            Some(Script::Super) => self.out.push_str("<w:vertAlign w:val=\"superscript\"/>"),
            Some(Script::Sub) => self.out.push_str("<w:vertAlign w:val=\"subscript\"/>"),
            None => {}
        }
        self.out.push_str("</w:rPr>");
        // Tabs and line ends are elements of their own in Word.
        let mut piece = String::new();
        for c in text.chars() {
            if c == '\t' || c == '\n' {
                self.text(&piece);
                piece.clear();
                self.out
                    .push_str(if c == '\t' { "<w:tab/>" } else { "<w:br/>" });
            } else {
                piece.push(c);
            }
        }
        self.text(&piece);
        self.out.push_str("</w:r>");
    }

    fn text(&mut self, text: &str) {
        if !text.is_empty() {
            self.out.push_str("<w:t xml:space=\"preserve\">");
            escape(&mut self.out, text);
            self.out.push_str("</w:t>");
        }
    }

    fn table(&mut self, rows: &[Row], context: &Context) {
        let room = (context.room - context.indent() as f64 / 20.0).max(72.0);
        let grid = Grid::new(rows, room);
        let total: f64 = grid.widths.iter().sum();
        let _ = write!(
            self.out,
            "<w:tbl><w:tblPr><w:tblStyle w:val=\"TableGrid\"/><w:tblW w:w=\"{}\" w:type=\"dxa\"/>",
            twips(total)
        );
        if context.indent() > 0 {
            let _ = write!(
                self.out,
                "<w:tblInd w:w=\"{}\" w:type=\"dxa\"/>",
                context.indent()
            );
        }
        self.out
            .push_str("<w:tblLayout w:type=\"fixed\"/></w:tblPr><w:tblGrid>");
        for width in &grid.widths {
            let _ = write!(self.out, "<w:gridCol w:w=\"{}\"/>", twips(*width));
        }
        self.out.push_str("</w:tblGrid>");
        for (index, row) in grid.rows.iter().enumerate() {
            self.out.push_str("<w:tr>");
            // Rows of heading cells at the top repeat on each page.
            let heading = rows[..=index]
                .iter()
                .all(|row| !row.cells.is_empty() && row.cells.iter().all(|cell| cell.header));
            let height = rows[index].height;
            if heading || height.is_some() {
                self.out.push_str("<w:trPr>");
                if let Some(height) = height {
                    let _ = write!(
                        self.out,
                        "<w:trHeight w:val=\"{height}\" w:hRule=\"atLeast\"/>"
                    );
                }
                if heading {
                    self.out.push_str("<w:tblHeader/>");
                }
                self.out.push_str("</w:trPr>");
            }
            let mut column = 0;
            for slot in row {
                let span = match slot {
                    Slot::Cell { span, .. } | Slot::Below { span } => *span,
                    Slot::Empty => 1,
                };
                let _ = write!(
                    self.out,
                    "<w:tc><w:tcPr><w:tcW w:w=\"{}\" w:type=\"dxa\"/>",
                    twips(grid.width(column, span))
                );
                if span > 1 {
                    let _ = write!(self.out, "<w:gridSpan w:val=\"{span}\"/>");
                }
                match slot {
                    Slot::Cell { cell, rows, .. } => {
                        if *rows > 1 {
                            self.out.push_str("<w:vMerge w:val=\"restart\"/>");
                        }
                        if cell.borders != [None; 4] {
                            let color = cell
                                .border_color
                                .map_or("000000".to_owned(), |color| color.hex_upper());
                            self.out.push_str("<w:tcBorders>");
                            // Word's order: top, left, bottom, right.
                            for at in [0, 3, 2, 1] {
                                let side = model::SIDES[at];
                                match cell.borders[at] {
                                    None => {}
                                    Some(0) => {
                                        let _ = write!(self.out, "<w:{side} w:val=\"nil\"/>");
                                    }
                                    Some(border) => {
                                        let _ = write!(
                                            self.out,
                                            "<w:{side} w:val=\"single\" w:sz=\"{}\" w:space=\"0\" w:color=\"{color}\"/>",
                                            eighths(border)
                                        );
                                    }
                                }
                            }
                            self.out.push_str("</w:tcBorders>");
                        }
                        // A cell's own color, or a heading cell's gray.
                        let fill = match cell.background {
                            Some(color) => Some(color.hex_upper()),
                            None if cell.header => Some("F1F3F4".to_owned()),
                            None => None,
                        };
                        if let Some(fill) = fill {
                            let _ = write!(
                                self.out,
                                "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{fill}\"/>"
                            );
                        }
                        if let Some(padding) = cell.padding {
                            self.out.push_str("<w:tcMar>");
                            for at in [0, 3, 2, 1] {
                                let _ = write!(
                                    self.out,
                                    "<w:{} w:w=\"{}\" w:type=\"dxa\"/>",
                                    model::SIDES[at],
                                    padding[at]
                                );
                            }
                            self.out.push_str("</w:tcMar>");
                        }
                        self.out.push_str("</w:tcPr>");
                        let across = cell.padding.map_or(10.8, |padding| {
                            model::points(padding[1]) + model::points(padding[3])
                        });
                        let mut inner = Context::top(grid.width(column, span) - across);
                        inner.header = cell.header;
                        self.blocks(&cell.content, &inner);
                        // A cell ends with a paragraph, even after a table.
                        if matches!(cell.content.last(), None | Some(Block::Table(_))) {
                            self.out.push_str("<w:p/>");
                        }
                    }
                    Slot::Below { .. } => self.out.push_str("<w:vMerge/></w:tcPr><w:p/>"),
                    Slot::Empty => self.out.push_str("</w:tcPr><w:p/>"),
                }
                self.out.push_str("</w:tc>");
                column += span;
            }
            self.out.push_str("</w:tr>");
        }
        self.out.push_str("</w:tbl>");
    }
}

/// A line's thickness in eighths of a point, as Word gives borders, from
/// a quarter point to Word's thickest, 12 points.
fn eighths(thickness: model::Twips) -> i32 {
    (thickness * 2 / 5).clamp(2, 96)
}

/// The numbering part: one definition each for bullets, checklists, and
/// numbers, and a numbering of its own for each numbered list, so each
/// starts at its own number.
fn numbering(numbered: &[(usize, u32, Option<ListStyle>)]) -> String {
    let mut out = String::from(XML);
    out.push_str(
        "<w:numbering xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
    );
    let bullets = ["•", "◦", "▪"];
    for (id, kind) in [
        (1, "bullets"),
        (2, "unchecked"),
        (3, "checked"),
        (4, "numbers"),
    ] {
        let _ = write!(
            out,
            "<w:abstractNum w:abstractNumId=\"{id}\"><w:multiLevelType w:val=\"hybridMultilevel\"/>"
        );
        for level in 0..=DEEPEST {
            let (format, text) = match kind {
                "bullets" => ("bullet", bullets[level % 3].to_owned()),
                "unchecked" => ("bullet", "☐".to_owned()),
                "checked" => ("bullet", "☑".to_owned()),
                // 1, 2, 3 at every level, as browsers show them.
                _ => ("decimal", format!("%{}.", level + 1)),
            };
            let _ = write!(
                out,
                "<w:lvl w:ilvl=\"{level}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"{format}\"/>\
                 <w:lvlText w:val=\"{text}\"/><w:lvlJc w:val=\"left\"/>\
                 <w:pPr><w:ind w:left=\"{}\" w:hanging=\"360\"/></w:pPr>",
                (level + 1) * 720
            );
            if format == "bullet" && kind != "bullets" {
                // Symbols that every font doesn't have.
                out.push_str("<w:rPr><w:rFonts w:ascii=\"Segoe UI Symbol\" w:hAnsi=\"Segoe UI Symbol\"/></w:rPr>");
            }
            out.push_str("</w:lvl>");
        }
        out.push_str("</w:abstractNum>");
    }
    // A definition for each list style, the same at every level.
    for style in ListStyle::ALL {
        let id = styled(style);
        let _ = write!(
            out,
            "<w:abstractNum w:abstractNumId=\"{id}\"><w:multiLevelType w:val=\"hybridMultilevel\"/>"
        );
        for level in 0..=DEEPEST {
            let (format, text) = match style {
                ListStyle::Decimal => ("decimal", format!("%{}.", level + 1)),
                ListStyle::DecimalParen => ("decimal", format!("%{})", level + 1)),
                ListStyle::LowerAlpha => ("lowerLetter", format!("%{}.", level + 1)),
                ListStyle::UpperAlpha => ("upperLetter", format!("%{}.", level + 1)),
                ListStyle::LowerRoman => ("lowerRoman", format!("%{}.", level + 1)),
                ListStyle::UpperRoman => ("upperRoman", format!("%{}.", level + 1)),
                bullet => ("bullet", bullet.bullet().unwrap_or('•').to_string()),
            };
            let _ = write!(
                out,
                "<w:lvl w:ilvl=\"{level}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"{format}\"/>\
                 <w:lvlText w:val=\"{text}\"/><w:lvlJc w:val=\"left\"/>\
                 <w:pPr><w:ind w:left=\"{}\" w:hanging=\"360\"/></w:pPr>",
                (level + 1) * 720
            );
            if matches!(style, ListStyle::Arrow | ListStyle::Star) {
                out.push_str("<w:rPr><w:rFonts w:ascii=\"Segoe UI Symbol\" w:hAnsi=\"Segoe UI Symbol\"/></w:rPr>");
            }
            out.push_str("</w:lvl>");
        }
        out.push_str("</w:abstractNum>");
    }
    for (id, abstract_id) in [(BULLETS, 1), (UNCHECKED, 2), (CHECKED, 3)] {
        let _ = write!(
            out,
            "<w:num w:numId=\"{id}\"><w:abstractNumId w:val=\"{abstract_id}\"/></w:num>"
        );
    }
    for style in ListStyle::ALL.into_iter().filter(|style| !style.numbered()) {
        let id = styled(style);
        let _ = write!(
            out,
            "<w:num w:numId=\"{id}\"><w:abstractNumId w:val=\"{id}\"/></w:num>"
        );
    }
    for (index, (level, start, style)) in numbered.iter().enumerate() {
        let _ = write!(
            out,
            "<w:num w:numId=\"{}\"><w:abstractNumId w:val=\"{}\"/>\
             <w:lvlOverride w:ilvl=\"{level}\"><w:startOverride w:val=\"{start}\"/></w:lvlOverride></w:num>",
            FIRST_NUMBERED + index,
            style.map_or(4, styled)
        );
    }
    out.push_str("</w:numbering>");
    out
}

const CONTENT_TYPES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
<Override PartName=\"/word/numbering.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/>\
<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>\
</Types>";

const PACKAGE_RELATIONS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/>\
</Relationships>";

/// The bookmark a table of contents entry links to, as Word names them.
fn toc_name(entry: usize) -> String {
    format!("_Toc{}", entry + 1)
}

/// The styles, matching the page Docs writes: Arial 11 point, headings
/// from 20 point down, quotes with a bar at their left, and code on gray.
const STYLES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Arial\" w:hAnsi=\"Arial\" w:eastAsia=\"Arial\" w:cs=\"Arial\"/>\
<w:color w:val=\"1F1F1F\"/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/></w:rPr></w:rPrDefault>\
<w:pPrDefault><w:pPr><w:spacing w:after=\"120\" w:line=\"276\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults>\
<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>\
<w:style w:type=\"character\" w:default=\"1\" w:styleId=\"DefaultParagraphFont\"><w:name w:val=\"Default Paragraph Font\"/><w:uiPriority w:val=\"1\"/><w:semiHidden/></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Title\"><w:name w:val=\"Title\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"0\" w:after=\"60\"/></w:pPr><w:rPr><w:sz w:val=\"52\"/><w:szCs w:val=\"52\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Subtitle\"><w:name w:val=\"Subtitle\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"0\" w:after=\"320\"/></w:pPr><w:rPr><w:color w:val=\"666666\"/><w:sz w:val=\"30\"/><w:szCs w:val=\"30\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading1\"><w:name w:val=\"heading 1\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"400\" w:after=\"120\"/><w:outlineLvl w:val=\"0\"/></w:pPr><w:rPr><w:sz w:val=\"40\"/><w:szCs w:val=\"40\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading2\"><w:name w:val=\"heading 2\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"360\" w:after=\"120\"/><w:outlineLvl w:val=\"1\"/></w:pPr><w:rPr><w:sz w:val=\"32\"/><w:szCs w:val=\"32\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading3\"><w:name w:val=\"heading 3\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"320\" w:after=\"80\"/><w:outlineLvl w:val=\"2\"/></w:pPr><w:rPr><w:color w:val=\"434343\"/><w:sz w:val=\"28\"/><w:szCs w:val=\"28\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading4\"><w:name w:val=\"heading 4\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"280\" w:after=\"80\"/><w:outlineLvl w:val=\"3\"/></w:pPr><w:rPr><w:color w:val=\"666666\"/><w:sz w:val=\"24\"/><w:szCs w:val=\"24\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading5\"><w:name w:val=\"heading 5\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"240\" w:after=\"80\"/><w:outlineLvl w:val=\"4\"/></w:pPr><w:rPr><w:color w:val=\"666666\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading6\"><w:name w:val=\"heading 6\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"240\" w:after=\"80\"/><w:outlineLvl w:val=\"5\"/></w:pPr><w:rPr><w:i/><w:color w:val=\"666666\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"FootnoteText\"><w:name w:val=\"footnote text\"/><w:basedOn w:val=\"Normal\"/>\
<w:pPr><w:spacing w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr><w:rPr><w:sz w:val=\"20\"/><w:szCs w:val=\"20\"/></w:rPr></w:style>\
<w:style w:type=\"character\" w:styleId=\"FootnoteReference\"><w:name w:val=\"footnote reference\"/>\
<w:rPr><w:vertAlign w:val=\"superscript\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Quote\"><w:name w:val=\"Quote\"/><w:basedOn w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:pBdr><w:left w:val=\"single\" w:sz=\"18\" w:space=\"8\" w:color=\"DADCE0\"/></w:pBdr></w:pPr><w:rPr><w:color w:val=\"444746\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Code\"><w:name w:val=\"Code\"/><w:basedOn w:val=\"Normal\"/>\
<w:pPr><w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"F1F3F4\"/><w:spacing w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr>\
<w:rPr><w:rFonts w:ascii=\"Courier New\" w:hAnsi=\"Courier New\" w:cs=\"Courier New\"/><w:sz w:val=\"20\"/><w:szCs w:val=\"20\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"TOC1\"><w:name w:val=\"toc 1\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:uiPriority w:val=\"39\"/><w:unhideWhenUsed/><w:pPr><w:spacing w:after=\"80\"/></w:pPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"TOC2\"><w:name w:val=\"toc 2\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:uiPriority w:val=\"39\"/><w:unhideWhenUsed/><w:pPr><w:spacing w:after=\"80\"/><w:ind w:left=\"360\"/></w:pPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"TOC3\"><w:name w:val=\"toc 3\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:uiPriority w:val=\"39\"/><w:unhideWhenUsed/><w:pPr><w:spacing w:after=\"80\"/><w:ind w:left=\"720\"/></w:pPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"TOC4\"><w:name w:val=\"toc 4\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:uiPriority w:val=\"39\"/><w:unhideWhenUsed/><w:pPr><w:spacing w:after=\"80\"/><w:ind w:left=\"1080\"/></w:pPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"TOC5\"><w:name w:val=\"toc 5\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:uiPriority w:val=\"39\"/><w:unhideWhenUsed/><w:pPr><w:spacing w:after=\"80\"/><w:ind w:left=\"1440\"/></w:pPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"TOC6\"><w:name w:val=\"toc 6\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:uiPriority w:val=\"39\"/><w:unhideWhenUsed/><w:pPr><w:spacing w:after=\"80\"/><w:ind w:left=\"1800\"/></w:pPr></w:style>\
<w:style w:type=\"character\" w:styleId=\"Hyperlink\"><w:name w:val=\"Hyperlink\"/><w:basedOn w:val=\"DefaultParagraphFont\"/>\
<w:rPr><w:color w:val=\"1155CC\"/><w:u w:val=\"single\"/></w:rPr></w:style>\
<w:style w:type=\"table\" w:default=\"1\" w:styleId=\"TableNormal\"><w:name w:val=\"Normal Table\"/><w:semiHidden/>\
<w:tblPr><w:tblInd w:w=\"0\" w:type=\"dxa\"/><w:tblCellMar><w:top w:w=\"0\" w:type=\"dxa\"/><w:left w:w=\"108\" w:type=\"dxa\"/>\
<w:bottom w:w=\"0\" w:type=\"dxa\"/><w:right w:w=\"108\" w:type=\"dxa\"/></w:tblCellMar></w:tblPr></w:style>\
<w:style w:type=\"table\" w:styleId=\"TableGrid\"><w:name w:val=\"Table Grid\"/><w:basedOn w:val=\"TableNormal\"/>\
<w:pPr><w:spacing w:after=\"0\"/></w:pPr>\
<w:tblPr><w:tblBorders><w:top w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"C4C7C5\"/><w:left w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"C4C7C5\"/>\
<w:bottom w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"C4C7C5\"/><w:right w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"C4C7C5\"/>\
<w:insideH w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"C4C7C5\"/><w:insideV w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"C4C7C5\"/></w:tblBorders>\
<w:tblCellMar><w:top w:w=\"60\" w:type=\"dxa\"/><w:left w:w=\"108\" w:type=\"dxa\"/><w:bottom w:w=\"60\" w:type=\"dxa\"/><w:right w:w=\"108\" w:type=\"dxa\"/></w:tblCellMar></w:tblPr></w:style>\
</w:styles>";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, PageSize};
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

    #[test]
    fn writes_contents_as_words() {
        let blocks = crate::html::tests::with_contents();
        let file = write(
            &blocks,
            &Parts::default(),
            None,
            "Contents",
            &Pictures::new(),
            &[1, 1, 2],
        );
        let document = crate::office::tests::unzip(&file, "word/document.xml");
        assert!(
            document.contains("<w:docPartGallery w:val=\"Table of Contents\"/>"),
            "{document}"
        );
        assert!(
            document.contains(" TOC \\o \"1-6\" \\h \\z \\u </w:instrText>"),
            "{document}"
        );
        assert!(
            document.contains("<w:pStyle w:val=\"TOC3\"/><w:tabs><w:tab w:val=\"right\" w:leader=\"dot\" w:pos=\"9360\"/>"),
            "{document}"
        );
        assert!(document.contains(" PAGEREF _Toc3 \\h </w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>2</w:t>"), "{document}");
        assert!(
            document.contains("<w:bookmarkStart w:id=\"1\" w:name=\"_Toc2\"/>"),
            "{document}"
        );
        assert_eq!(document.matches("<w:bookmarkEnd ").count(), 3, "{document}");
        // Page numbers that don't match the entries are left out.
        let file = write(
            &blocks,
            &Parts::default(),
            None,
            "Contents",
            &Pictures::new(),
            &[1],
        );
        let document = crate::office::tests::unzip(&file, "word/document.xml");
        assert!(
            document.contains("<w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t></w:t>"),
            "{document}"
        );
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
        let document = crate::office::tests::unzip(&file, "word/document.xml");
        assert!(
            document.contains("<w:footnoteReference w:id=\"1\"/>")
                && document.contains("<w:footnoteReference w:id=\"2\"/>"),
            "{document}"
        );
        let notes = crate::office::tests::unzip(&file, "word/footnotes.xml");
        assert!(
            notes.contains("<w:footnote w:type=\"separator\" w:id=\"-1\">"),
            "{notes}"
        );
        assert!(
            notes.contains("<w:footnote w:id=\"1\"><w:p><w:pPr><w:pStyle w:val=\"FootnoteText\"/>"),
            "{notes}"
        );
        assert!(
            notes.contains("<w:b/>") && notes.contains(">Two</w:t>"),
            "{notes}"
        );
        let relations = crate::office::tests::unzip(&file, "word/_rels/document.xml.rels");
        assert!(
            relations.contains("Target=\"footnotes.xml\""),
            "{relations}"
        );
        let types = crate::office::tests::unzip(&file, "[Content_Types].xml");
        assert!(types.contains("/word/footnotes.xml"), "{types}");
        assert!(
            crate::office::tests::unzip(&file, "word/styles.xml").contains("FootnoteReference")
        );
        // A document without footnotes has no footnotes part.
        let plain = write(&[], &Parts::default(), None, "Plain", &Pictures::new(), &[]);
        let types = crate::office::tests::unzip(&plain, "[Content_Types].xml");
        assert!(!types.contains("footnotes"), "{types}");
    }

    #[test]
    fn writes_headers_footers_and_page_numbers() {
        let parts = crate::html::tests::parts();
        let blocks = vec![Block::Paragraph(Para::default(), Vec::new())];
        let file = write(&blocks, &parts, None, "Parts", &Pictures::new(), &[]);
        let document = crate::office::tests::unzip(&file, "word/document.xml");
        assert!(
            document.contains("<w:headerReference w:type=\"default\" r:id=\"rId-header\"/>"),
            "{document}"
        );
        assert!(
            document.contains("<w:footerReference w:type=\"first\" r:id=\"rId-first-footer\"/>"),
            "{document}"
        );
        assert!(document.contains("<w:titlePg/></w:sectPr>"), "{document}");
        let header = crate::office::tests::unzip(&file, "word/header.xml");
        assert!(
            header.starts_with(XML) && header.contains("<w:hdr "),
            "{header}"
        );
        assert!(
            header.contains("<w:fldSimple w:instr=\" PAGE \"><w:r><w:rPr><w:sz w:val=\"18\"/>"),
            "{header}"
        );
        assert!(
            header.contains("<w:fldSimple w:instr=\" NUMPAGES \">"),
            "{header}"
        );
        let relations = crate::office::tests::unzip(&file, "word/_rels/document.xml.rels");
        assert!(
            relations.contains("Target=\"first-footer.xml\""),
            "{relations}"
        );
        let types = crate::office::tests::unzip(&file, "[Content_Types].xml");
        assert!(types.contains("PartName=\"/word/footer.xml\""), "{types}");
        crate::office::tests::unzip(&file, "word/_rels/header.xml.rels");
    }

    #[test]
    fn writes_a_word_file_on_the_documents_paper() {
        let link = Marks {
            link: Some("https://example.com/?a=1&b=2".to_owned()),
            ..Marks::default()
        };
        let blocks = vec![
            Block::Heading(
                1,
                Para::default(),
                vec![Inline::Text("Plan".to_owned(), Marks::default())],
            ),
            Block::Paragraph(
                Para::default(),
                vec![
                    Inline::Text("See ".to_owned(), Marks::default()),
                    Inline::Text("the site".to_owned(), link),
                    Inline::Text(
                        "styled".to_owned(),
                        Marks {
                            color: Some(crate::style::Color([0xcc, 0, 0])),
                            highlight: Some(crate::style::Color([0xff, 0xf2, 0xcc])),
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
                vec![vec![Block::Paragraph(
                    Para::default(),
                    vec![Inline::Text("Third".to_owned(), Marks::default())],
                )]],
            ),
            Block::Checks(vec![(
                true,
                vec![Block::Paragraph(
                    Para::default(),
                    vec![Inline::Text("Done".to_owned(), Marks::default())],
                )],
            )]),
            crate::model::table(vec![vec![Cell {
                header: true,
                colspan: 1,
                rowspan: 1,
                colwidth: None,
                background: None,
                content: vec![Block::Paragraph(
                    Para::default(),
                    vec![Inline::Text("Head".to_owned(), Marks::default())],
                )],
                ..Cell::default()
            }]]),
        ];
        let setup = PageSetup {
            size: PageSize::A4,
            landscape: true,
            margins: [36.0, 72.0, 36.0, 72.0],
        };
        let file = write(
            &blocks,
            &crate::model::Parts::default(),
            Some(&setup),
            "Plan & notes",
            &Pictures::new(),
            &[],
        );
        let document = part(&file, "word/document.xml");
        assert!(
            document.contains("<w:pStyle w:val=\"Heading1\"/>"),
            "{document}"
        );
        assert!(
            document.contains("<w:hyperlink r:id=\"rIdLink0\">"),
            "{document}"
        );
        assert!(document.contains("<w:numId w:val=\"4\"/>"), "{document}");
        assert!(document.contains("<w:numId w:val=\"3\"/>"), "{document}");
        assert!(document.contains("<w:strike/>"), "{document}");
        assert!(document.contains("<w:tblHeader/>"), "{document}");
        assert!(document.contains("<w:rFonts w:ascii=\"Open Sans\" w:hAnsi=\"Open Sans\" w:eastAsia=\"Open Sans\" w:cs=\"Open Sans\"/><w:color w:val=\"CC0000\"/><w:sz w:val=\"21\"/><w:szCs w:val=\"21\"/><w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"FFF2CC\"/><w:vertAlign w:val=\"superscript\"/>"), "{document}");
        // A4 landscape: 297 by 210 mm.
        assert!(
            document.contains("<w:pgSz w:w=\"16838\" w:h=\"11906\" w:orient=\"landscape\"/>"),
            "{document}"
        );
        assert!(
            document.contains("w:top=\"720\" w:right=\"1440\""),
            "{document}"
        );
        let relations = part(&file, "word/_rels/document.xml.rels");
        assert!(
            relations.contains("Target=\"https://example.com/?a=1&amp;b=2\""),
            "{relations}"
        );
        let numbering = part(&file, "word/numbering.xml");
        assert!(
            numbering.contains("<w:num w:numId=\"4\"><w:abstractNumId w:val=\"4\"/><w:lvlOverride w:ilvl=\"0\"><w:startOverride w:val=\"3\"/>"),
            "{numbering}"
        );
        assert!(part(&file, "docProps/core.xml").contains("<dc:title>Plan &amp; notes</dc:title>"));

        // Pictures go in the file, once each, sized to fit the page.
        let picture = Picture::read(crate::picture::tests::png(4000, 1000)).unwrap();
        let image = Block::Image(Image {
            hash: picture.hash.clone(),
            alt: "Wide".into(),
            width: None,
        });
        let pictures: Pictures = [(picture.hash.clone(), picture)].into_iter().collect();
        let file = write(
            &[image.clone(), image],
            &crate::model::Parts::default(),
            Some(&setup),
            "Pictures",
            &pictures,
            &[],
        );
        let document = part(&file, "word/document.xml");
        assert_eq!(
            document.matches("r:embed=\"rIdPicture0\"").count(),
            2,
            "{document}"
        );
        // A4 landscape less 2 inches of margins is 697.89 points wide.
        assert!(
            document.contains("<wp:extent cx=\"8863200\" cy=\"2215800\"/>"),
            "{document}"
        );
        assert!(document.contains("descr=\"Wide\""), "{document}");
        assert!(
            part(&file, "[Content_Types].xml")
                .contains("<Default Extension=\"png\" ContentType=\"image/png\"/>")
        );
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&file[..])).unwrap();
        assert!(zip.by_name("word/media/picture0.png").is_ok());
        assert!(zip.by_name("word/media/picture1.png").is_err());
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
        let document = part(&file, "word/document.xml");
        assert!(document.contains("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"FCE5CD\"/>"));
        // A heading cell without a color of its own is gray.
        assert!(document.contains("w:fill=\"F1F3F4\""));
        assert!(document.contains(
            "<w:gridSpan w:val=\"2\"/><w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"CFE2F3\"/>"
        ));
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
        let document = part(&file, "word/document.xml");
        assert!(
            document.contains("<w:trPr><w:trHeight w:val=\"2400\" w:hRule=\"atLeast\"/></w:trPr>")
        );
        assert!(document.contains(
            "<w:tcBorders><w:top w:val=\"nil\"/><w:left w:val=\"nil\"/><w:bottom w:val=\"nil\"/><w:right w:val=\"nil\"/></w:tcBorders>\
             <w:tcMar><w:top w:w=\"72\" w:type=\"dxa\"/>"
        ));
        assert!(
            document
                .contains("<w:top w:val=\"single\" w:sz=\"8\" w:space=\"0\" w:color=\"FF0000\"/>")
        );
        assert!(
            document.contains(
                "<w:bottom w:val=\"single\" w:sz=\"18\" w:space=\"1\" w:color=\"000000\"/>"
            )
        );
        // Headings are regular weight, as in Google Docs.
        let styles = part(&file, "word/styles.xml");
        assert!(!styles.contains("<w:outlineLvl w:val=\"0\"/></w:pPr><w:rPr><w:b/>"));
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
        let document = part(&file, "word/document.xml");
        assert!(document.contains("<w:pStyle w:val=\"Title\"/><w:jc w:val=\"center\"/>"));
        assert!(document.contains(
            "<w:spacing w:before=\"240\" w:after=\"0\" w:line=\"360\" w:lineRule=\"auto\"/>\
             <w:ind w:left=\"720\" w:right=\"360\" w:hanging=\"360\"/><w:jc w:val=\"both\"/>"
        ));
        assert!(document.contains("<w:br w:type=\"page\"/>"));
        let numbering = part(&file, "word/numbering.xml");
        assert!(numbering.contains("<w:numFmt w:val=\"upperRoman\"/>"));
        assert!(numbering.contains("<w:lvlText w:val=\"➢\"/>"));
        assert!(numbering.contains("<w:lvlText w:val=\"%1)\"/>"));
        assert!(part(&file, "word/styles.xml").contains("w:styleId=\"Subtitle\""));
    }
}
