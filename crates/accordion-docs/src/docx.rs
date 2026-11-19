//! A document as a Word file (`.docx`), for **Download as** (decision 30).
//!
//! It keeps everything the editor has: headings, marks, links, lists to
//! nine levels, checklists (as ☐ and ☑ bullets, with done items struck
//! through as on the page), quotes, code, rules, and tables with merged
//! cells and column widths. The page setup becomes the section's paper and
//! margins. Text is Arial 11 point, as in Google Docs.

use std::fmt::Write as _;

use crate::model::{Block, Image, Inline, Marks, PageSetup};
use crate::office::{self, Grid, Part, Slot, escape, escaped};
use crate::picture::{Picture, Pictures};

/// `blocks` on `setup`'s page as a Word file, with `pictures` in it and
/// `title` in its properties.
pub fn write(
    blocks: &[Block],
    setup: Option<&PageSetup>,
    title: &str,
    pictures: &Pictures,
) -> Vec<u8> {
    let page = office::page_of(setup);
    let mut writer = Writer {
        out: String::new(),
        links: Vec::new(),
        numbered: Vec::new(),
        pictures,
        media: Vec::new(),
        drawings: 0,
    };
    let room = office::text_width(&page);
    writer.blocks(blocks, &Context::top(room));
    let body = std::mem::take(&mut writer.out);

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
        "<w:sectPr><w:pgSz w:w=\"{}\" w:h=\"{}\"{}/>\
         <w:pgMar w:top=\"{top}\" w:right=\"{right}\" w:bottom=\"{bottom}\" w:left=\"{left}\" \
         w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/></w:sectPr></w:body></w:document>",
        twips(width),
        twips(height),
        if page.landscape {
            " w:orient=\"landscape\""
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
    /// Each numbered list's level and first number, by its place after
    /// `FIRST_NUMBERED`.
    numbered: Vec<(usize, u32)>,
    pictures: &'a Pictures,
    /// Each picture in the file, by its place: `rIdPicture{index}`.
    media: Vec<Picture>,
    /// How many pictures are drawn, for their IDs.
    drawings: usize,
}

impl Writer<'_> {
    fn blocks(&mut self, blocks: &[Block], context: &Context) {
        let mut context = context.clone();
        let mut after_table = false;
        for block in blocks {
            // Two tables in a row would join into one in Word.
            if after_table && matches!(block, Block::Table(_)) {
                self.paragraph(&context, None, &[]);
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
            Block::Paragraph(inlines) => self.paragraph(context, None, inlines),
            Block::Heading(level, inlines) => {
                let style = format!("Heading{}", (*level).clamp(1, 6));
                self.paragraph(context, Some(&style), inlines);
            }
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
                    self.paragraph(context, Some("Code"), std::slice::from_ref(&code));
                }
            }
            Block::Rule => {
                self.out.push_str("<w:p><w:pPr><w:pBdr><w:bottom w:val=\"single\" w:sz=\"6\" w:space=\"1\" w:color=\"C4C7C5\"/></w:pBdr>");
                self.indent(context, false);
                self.out.push_str("</w:pPr></w:p>");
            }
            Block::Bullets(items) => {
                for item in items {
                    self.item(item, context, BULLETS, false);
                }
            }
            Block::Numbers(start, items) => {
                let level = context.lists.min(DEEPEST);
                self.numbered.push((level, *start));
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
        self.indent(context, false);
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
            self.paragraph(&inner, None, &[]);
        } else {
            self.blocks(blocks, &inner);
        }
    }

    /// Writes the indent for a paragraph inside quotes or lists. A list
    /// item's first paragraph hangs its bullet or number in the indent.
    fn indent(&mut self, context: &Context, hanging: bool) {
        let left = context.indent();
        if left == 0 {
            return;
        }
        if hanging {
            let _ = write!(self.out, "<w:ind w:left=\"{left}\" w:hanging=\"360\"/>");
        } else {
            let _ = write!(self.out, "<w:ind w:left=\"{left}\"/>");
        }
    }

    fn paragraph(&mut self, context: &Context, style: Option<&str>, inlines: &[Inline]) {
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
        if context.lists > 0 || context.quotes > 0 {
            // Paragraphs in a list sit close together, as on the page.
            if context.lists > 0 {
                self.out.push_str("<w:spacing w:after=\"40\"/>");
            }
            self.indent(context, context.item.is_some());
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
                Inline::Break => None,
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
                Inline::Text(text, marks) => self.run(text, marks, context),
                Inline::Break => self.out.push_str("<w:r><w:br/></w:r>"),
            }
        }
        if link.is_some() {
            self.out.push_str("</w:hyperlink>");
        }
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
        if marks.code {
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
        }
        if marks.underline {
            self.out.push_str("<w:u w:val=\"single\"/>");
        }
        if marks.code {
            self.out
                .push_str("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"F1F3F4\"/>");
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

    fn table(&mut self, rows: &[Vec<crate::model::Cell>], context: &Context) {
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
                .all(|row| !row.is_empty() && row.iter().all(|cell| cell.header));
            if heading {
                self.out.push_str("<w:trPr><w:tblHeader/></w:trPr>");
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
                        if cell.header {
                            self.out.push_str(
                                "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"F1F3F4\"/>",
                            );
                        }
                        self.out.push_str("</w:tcPr>");
                        let mut inner = Context::top(grid.width(column, span) - 10.8);
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

/// The numbering part: one definition each for bullets, checklists, and
/// numbers, and a numbering of its own for each numbered list, so each
/// starts at its own number.
fn numbering(numbered: &[(usize, u32)]) -> String {
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
    for (id, abstract_id) in [(BULLETS, 1), (UNCHECKED, 2), (CHECKED, 3)] {
        let _ = write!(
            out,
            "<w:num w:numId=\"{id}\"><w:abstractNumId w:val=\"{abstract_id}\"/></w:num>"
        );
    }
    for (index, (level, start)) in numbered.iter().enumerate() {
        let _ = write!(
            out,
            "<w:num w:numId=\"{}\"><w:abstractNumId w:val=\"4\"/>\
             <w:lvlOverride w:ilvl=\"{level}\"><w:startOverride w:val=\"{start}\"/></w:lvlOverride></w:num>",
            FIRST_NUMBERED + index
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

/// The styles, matching the page Docs writes: Arial 11 point, headings
/// from 20 point down, quotes with a bar at their left, and code on gray.
const STYLES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Arial\" w:hAnsi=\"Arial\" w:eastAsia=\"Arial\" w:cs=\"Arial\"/>\
<w:color w:val=\"1F1F1F\"/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/></w:rPr></w:rPrDefault>\
<w:pPrDefault><w:pPr><w:spacing w:after=\"120\" w:line=\"276\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults>\
<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>\
<w:style w:type=\"character\" w:default=\"1\" w:styleId=\"DefaultParagraphFont\"><w:name w:val=\"Default Paragraph Font\"/><w:uiPriority w:val=\"1\"/><w:semiHidden/></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading1\"><w:name w:val=\"heading 1\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"400\" w:after=\"120\"/><w:outlineLvl w:val=\"0\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"40\"/><w:szCs w:val=\"40\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading2\"><w:name w:val=\"heading 2\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"360\" w:after=\"120\"/><w:outlineLvl w:val=\"1\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"32\"/><w:szCs w:val=\"32\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading3\"><w:name w:val=\"heading 3\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"320\" w:after=\"80\"/><w:outlineLvl w:val=\"2\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"28\"/><w:szCs w:val=\"28\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading4\"><w:name w:val=\"heading 4\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"280\" w:after=\"80\"/><w:outlineLvl w:val=\"3\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"24\"/><w:szCs w:val=\"24\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading5\"><w:name w:val=\"heading 5\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"240\" w:after=\"80\"/><w:outlineLvl w:val=\"4\"/></w:pPr><w:rPr><w:b/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading6\"><w:name w:val=\"heading 6\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"240\" w:after=\"80\"/><w:outlineLvl w:val=\"5\"/></w:pPr><w:rPr><w:b/><w:i/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Quote\"><w:name w:val=\"Quote\"/><w:basedOn w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:pBdr><w:left w:val=\"single\" w:sz=\"18\" w:space=\"8\" w:color=\"DADCE0\"/></w:pBdr></w:pPr><w:rPr><w:color w:val=\"444746\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Code\"><w:name w:val=\"Code\"/><w:basedOn w:val=\"Normal\"/>\
<w:pPr><w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"F1F3F4\"/><w:spacing w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr>\
<w:rPr><w:rFonts w:ascii=\"Courier New\" w:hAnsi=\"Courier New\" w:cs=\"Courier New\"/><w:sz w:val=\"20\"/><w:szCs w:val=\"20\"/></w:rPr></w:style>\
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
    fn writes_a_word_file_on_the_documents_paper() {
        let link = Marks {
            link: Some("https://example.com/?a=1&b=2".to_owned()),
            ..Marks::default()
        };
        let blocks = vec![
            Block::Heading(1, vec![Inline::Text("Plan".to_owned(), Marks::default())]),
            Block::Paragraph(vec![
                Inline::Text("See ".to_owned(), Marks::default()),
                Inline::Text("the site".to_owned(), link),
            ]),
            Block::Numbers(
                3,
                vec![vec![Block::Paragraph(vec![Inline::Text(
                    "Third".to_owned(),
                    Marks::default(),
                )])]],
            ),
            Block::Checks(vec![(
                true,
                vec![Block::Paragraph(vec![Inline::Text(
                    "Done".to_owned(),
                    Marks::default(),
                )])],
            )]),
            Block::Table(vec![vec![Cell {
                header: true,
                colspan: 1,
                rowspan: 1,
                colwidth: None,
                content: vec![Block::Paragraph(vec![Inline::Text(
                    "Head".to_owned(),
                    Marks::default(),
                )])],
            }]]),
        ];
        let setup = PageSetup {
            size: PageSize::A4,
            landscape: true,
            margins: [36.0, 72.0, 36.0, 72.0],
        };
        let file = write(&blocks, Some(&setup), "Plan & notes", &Pictures::new());
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
        let file = write(&[image.clone(), image], Some(&setup), "Pictures", &pictures);
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
}
