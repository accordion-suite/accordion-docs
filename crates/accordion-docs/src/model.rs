//! A document as Docs understands it: the blocks and text the editor's
//! schema allows, and nothing else (decision 30).
//!
//! The editor in the browser (ProseMirror) and the server agree on this
//! schema. Its node and mark names are the ones the web app's schema uses,
//! since y-prosemirror stores them in the co-editing document:
//!
//! | Here | Editor node | HTML |
//! |---|---|---|
//! | [`Block::Paragraph`] | `paragraph` | `<p>` |
//! | [`Block::Heading`] | `heading` (`level`) | `<h1>` to `<h6>` |
//! | [`Block::Quote`] | `blockquote` | `<blockquote>` |
//! | [`Block::Code`] | `code_block` | `<pre><code>` |
//! | [`Block::Rule`] | `horizontal_rule` | `<hr>` |
//! | [`Block::Bullets`] | `bullet_list`, `list_item` | `<ul>`, `<li>` |
//! | [`Block::Numbers`] | `ordered_list` (`order`), `list_item` | `<ol>`, `<li>` |
//! | [`Block::Checks`] | `check_list`, `check_item` (`checked`) | `<ul class="checklist">`, `<li>` |
//! | [`Block::Table`] | `table`, `table_row`, `table_cell`, `table_header` | `<table>`, `<tr>`, `<td>`, `<th>` |
//! | [`Block::Image`] | `image` (`src`, `alt`, `width`) | `<img>` |
//! | [`Inline::Break`] | `hard_break` | `<br>` |
//!
//! Marks: `bold` (`<strong>`), `italic` (`<em>`), `underline` (`<u>`),
//! `strike` (`<s>`), `code` (`<code>`), and `link` (`<a href>`).
//!
//! [`normalize`] makes any list of blocks one the editor accepts, the way
//! ProseMirror's content rules would, so a document read from HTML or from a
//! co-editing state never makes the editor throw content away.

/// The marks on a run of text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marks {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub code: bool,
    /// The link's address, once it's been checked by [`safe_link`].
    pub link: Option<String>,
}

impl Marks {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inline {
    Text(String, Marks),
    /// A line break inside a paragraph.
    Break,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Inline>),
    /// Levels 1 to 6.
    Heading(u8, Vec<Inline>),
    Quote(Vec<Block>),
    /// Plain text, without marks.
    Code(String),
    Rule,
    /// Each item is a list of blocks that starts with a paragraph.
    Bullets(Vec<Vec<Block>>),
    /// The number the list starts at, and its items.
    Numbers(u32, Vec<Vec<Block>>),
    /// Whether each item is checked, and its blocks.
    Checks(Vec<(bool, Vec<Block>)>),
    /// Rows of cells.
    Table(Vec<Vec<Cell>>),
    /// A picture, on a line of its own.
    Image(Image),
}

/// A picture in a document. Its contents are kept apart from the blocks,
/// by their hash ([`crate::picture`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// The SHA-256 hash of the picture's contents, in lowercase hex. The
    /// editor's node keeps it as `src`.
    pub hash: String,
    /// Words that say what the picture shows, for people who can't see it.
    pub alt: String,
    /// How wide it's shown, in pixels, once someone has set it. Otherwise
    /// it's as wide as it is, or as the page.
    pub width: Option<u32>,
}

/// The longest alt text kept.
const MAX_ALT: usize = 1000;

/// The widest a picture is shown, in pixels: wider than any page.
const MAX_WIDTH: u32 = 5000;

/// The hash of every picture in `blocks`, each once, in order.
pub fn image_hashes(blocks: &[Block]) -> Vec<String> {
    fn walk(blocks: &[Block], out: &mut Vec<String>) {
        for block in blocks {
            match block {
                Block::Image(image) => {
                    if !out.contains(&image.hash) {
                        out.push(image.hash.clone());
                    }
                }
                Block::Quote(blocks) => walk(blocks, out),
                Block::Bullets(items) | Block::Numbers(_, items) => {
                    items.iter().for_each(|item| walk(item, out));
                }
                Block::Checks(items) => items.iter().for_each(|(_, item)| walk(item, out)),
                Block::Table(rows) => rows
                    .iter()
                    .flatten()
                    .for_each(|cell| walk(&cell.content, out)),
                Block::Paragraph(_) | Block::Heading(..) | Block::Code(_) | Block::Rule => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(blocks, &mut out);
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// A heading cell, `<th>`.
    pub header: bool,
    pub colspan: u32,
    pub rowspan: u32,
    /// Column widths in pixels, one for each column the cell spans, once
    /// someone has dragged a column's edge.
    pub colwidth: Option<Vec<u32>>,
    pub content: Vec<Block>,
}

/// A page size people can choose, as in Google Docs and Word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageSize {
    /// 8.5 by 11 inches, usual in the US, Canada, and Mexico.
    #[default]
    Letter,
    /// 210 by 297 mm, usual everywhere else.
    A4,
    /// 8.5 by 14 inches.
    Legal,
    /// 148 by 210 mm.
    A5,
    /// 297 by 420 mm.
    A3,
    /// 11 by 17 inches.
    Tabloid,
}

impl PageSize {
    pub const ALL: [PageSize; 6] = [
        PageSize::Letter,
        PageSize::A4,
        PageSize::Legal,
        PageSize::A5,
        PageSize::A3,
        PageSize::Tabloid,
    ];

    /// The name in files and in the co-editing document.
    pub fn name(self) -> &'static str {
        match self {
            PageSize::Letter => "letter",
            PageSize::A4 => "a4",
            PageSize::Legal => "legal",
            PageSize::A5 => "a5",
            PageSize::A3 => "a3",
            PageSize::Tabloid => "tabloid",
        }
    }

    pub fn from_name(name: &str) -> Option<PageSize> {
        Self::ALL.into_iter().find(|size| size.name() == name)
    }

    /// Width and height upright, as CSS lengths and in points.
    fn dimensions(self) -> ((&'static str, &'static str), (f64, f64)) {
        const MM: f64 = 72.0 / 25.4;
        match self {
            PageSize::Letter => (("8.5in", "11in"), (612.0, 792.0)),
            PageSize::A4 => (("210mm", "297mm"), (210.0 * MM, 297.0 * MM)),
            PageSize::Legal => (("8.5in", "14in"), (612.0, 1008.0)),
            PageSize::A5 => (("148mm", "210mm"), (148.0 * MM, 210.0 * MM)),
            PageSize::A3 => (("297mm", "420mm"), (297.0 * MM, 420.0 * MM)),
            PageSize::Tabloid => (("11in", "17in"), (792.0, 1224.0)),
        }
    }
}

/// A document's page: its size, which way up, and its margins, for printing
/// and for how wide the editor's page is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageSetup {
    pub size: PageSize,
    pub landscape: bool,
    /// Top, right, bottom, and left, in points (72 to the inch).
    pub margins: [f64; 4],
}

/// One inch, the usual margin in Google Docs and Word.
pub const DEFAULT_MARGIN: f64 = 72.0;

/// The most a margin can be: 4 inches.
pub const MAX_MARGIN: f64 = 288.0;

/// The least room the margins leave for text, across and down: 1 inch.
const MIN_TEXT: f64 = 72.0;

impl PageSetup {
    /// A page of `size`, upright, with 1-inch margins.
    pub fn new(size: PageSize) -> Self {
        Self {
            size,
            landscape: false,
            margins: [DEFAULT_MARGIN; 4],
        }
    }

    /// Width and height as laid out, as CSS lengths.
    pub fn css_size(&self) -> (&'static str, &'static str) {
        let ((width, height), _) = self.size.dimensions();
        if self.landscape {
            (height, width)
        } else {
            (width, height)
        }
    }

    /// Width and height as laid out, in points.
    pub fn points(&self) -> (f64, f64) {
        let (_, (width, height)) = self.size.dimensions();
        if self.landscape {
            (height, width)
        } else {
            (width, height)
        }
    }

    /// The same page with margins that fit: none below zero or above 4
    /// inches, rounded to a hundredth of a point, and at least an inch of
    /// room for text each way.
    pub fn normalized(mut self) -> Self {
        for margin in &mut self.margins {
            let value = if margin.is_finite() {
                *margin
            } else {
                DEFAULT_MARGIN
            };
            *margin = (value.clamp(0.0, MAX_MARGIN) * 100.0).round() / 100.0;
        }
        let (width, height) = self.points();
        let [top, right, bottom, left] = &mut self.margins;
        shrink_pair(left, right, width - MIN_TEXT);
        shrink_pair(top, bottom, height - MIN_TEXT);
        self
    }
}

/// Shrinks two margins in proportion until they add up to at most `room`.
fn shrink_pair(a: &mut f64, b: &mut f64, room: f64) {
    let total = *a + *b;
    if total > room && total > 0.0 {
        let scale = room / total;
        *a = (*a * scale * 100.0).floor() / 100.0;
        *b = (*b * scale * 100.0).floor() / 100.0;
    }
}

/// The most rows or columns a cell can span. More is a mistake, or a file
/// built to make the editor slow.
pub const MAX_SPAN: u32 = 1000;

/// Makes `blocks` a whole document: at least one block, each valid.
pub fn normalize(blocks: Vec<Block>) -> Vec<Block> {
    let blocks = normalize_blocks(blocks);
    if blocks.is_empty() {
        vec![Block::Paragraph(Vec::new())]
    } else {
        blocks
    }
}

/// Fixes each block, and drops the ones with nothing left, such as a list
/// without items.
pub fn normalize_blocks(blocks: Vec<Block>) -> Vec<Block> {
    blocks.into_iter().filter_map(normalize_block).collect()
}

/// One block as the editor would accept it, or `None` if nothing of it is
/// left.
pub fn normalize_block(block: Block) -> Option<Block> {
    Some(match block {
        Block::Paragraph(inlines) => Block::Paragraph(normalize_inlines(inlines)),
        Block::Heading(level, inlines) => {
            Block::Heading(level.clamp(1, 6), normalize_inlines(inlines))
        }
        Block::Quote(blocks) => Block::Quote(at_least_a_paragraph(blocks)),
        Block::Code(text) => Block::Code(text),
        Block::Rule => Block::Rule,
        Block::Image(image) => {
            if !crate::picture::is_hash(&image.hash) {
                return None;
            }
            let alt: String = image
                .alt
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_ALT)
                .collect();
            Block::Image(Image {
                hash: image.hash,
                alt: alt.trim().to_owned(),
                width: image.width.filter(|width| (1..=MAX_WIDTH).contains(width)),
            })
        }
        Block::Bullets(items) => {
            let items: Vec<_> = items.into_iter().map(item).collect();
            if items.is_empty() {
                return None;
            }
            Block::Bullets(items)
        }
        Block::Numbers(start, items) => {
            let items: Vec<_> = items.into_iter().map(item).collect();
            if items.is_empty() {
                return None;
            }
            Block::Numbers(start, items)
        }
        Block::Checks(items) => {
            let items: Vec<_> = items
                .into_iter()
                .map(|(checked, blocks)| (checked, item(blocks)))
                .collect();
            if items.is_empty() {
                return None;
            }
            Block::Checks(items)
        }
        Block::Table(rows) => {
            let rows: Vec<Vec<Cell>> = rows
                .into_iter()
                .map(|row| row.into_iter().map(cell).collect::<Vec<_>>())
                .filter(|row| !row.is_empty())
                .collect();
            if rows.is_empty() {
                return None;
            }
            Block::Table(rows)
        }
    })
}

/// A list item: blocks that start with a paragraph.
fn item(blocks: Vec<Block>) -> Vec<Block> {
    let mut blocks = normalize_blocks(blocks);
    if !matches!(blocks.first(), Some(Block::Paragraph(_))) {
        blocks.insert(0, Block::Paragraph(Vec::new()));
    }
    blocks
}

fn cell(cell: Cell) -> Cell {
    let colspan = cell.colspan.clamp(1, MAX_SPAN);
    let colwidth = cell
        .colwidth
        .filter(|widths| widths.len() == colspan as usize && widths.iter().all(|w| *w > 0));
    Cell {
        header: cell.header,
        colspan,
        rowspan: cell.rowspan.clamp(1, MAX_SPAN),
        colwidth,
        content: at_least_a_paragraph(cell.content),
    }
}

fn at_least_a_paragraph(blocks: Vec<Block>) -> Vec<Block> {
    let blocks = normalize_blocks(blocks);
    if blocks.is_empty() {
        vec![Block::Paragraph(Vec::new())]
    } else {
        blocks
    }
}

/// Joins runs with the same marks and drops empty ones, as ProseMirror
/// does.
pub fn normalize_inlines(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::with_capacity(inlines.len());
    for inline in inlines {
        match inline {
            Inline::Text(text, _) if text.is_empty() => {}
            Inline::Text(text, marks) => match out.last_mut() {
                Some(Inline::Text(last, last_marks)) if *last_marks == marks => {
                    last.push_str(&text);
                }
                _ => out.push(Inline::Text(text, marks)),
            },
            Inline::Break => out.push(Inline::Break),
        }
    }
    out
}

/// A link's address if it's one people can follow safely: a web page or an
/// email address. Anything else, such as `javascript:`, is dropped, and the
/// text stays without a link.
pub fn safe_link(href: &str) -> Option<String> {
    let href = href.trim();
    let lower = href.to_ascii_lowercase();
    let allowed = ["http://", "https://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme));
    (allowed && href.len() <= 2048 && !href.chars().any(char::is_control)).then(|| href.to_owned())
}

/// The plain text of the first block with any, for the page's title.
pub fn first_text(blocks: &[Block]) -> Option<String> {
    for block in blocks {
        let text = match block {
            Block::Paragraph(inlines) | Block::Heading(_, inlines) => plain(inlines),
            Block::Quote(blocks) => first_text(blocks).unwrap_or_default(),
            Block::Code(text) => text.clone(),
            Block::Bullets(items) | Block::Numbers(_, items) => items
                .iter()
                .find_map(|blocks| first_text(blocks))
                .unwrap_or_default(),
            Block::Checks(items) => items
                .iter()
                .find_map(|(_, blocks)| first_text(blocks))
                .unwrap_or_default(),
            Block::Table(rows) => rows
                .iter()
                .flatten()
                .find_map(|cell| first_text(&cell.content))
                .unwrap_or_default(),
            Block::Rule | Block::Image(_) => String::new(),
        };
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

fn plain(inlines: &[Inline]) -> String {
    inlines
        .iter()
        .map(|inline| match inline {
            Inline::Text(text, _) => text.as_str(),
            Inline::Break => " ",
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> Inline {
        Inline::Text(text.into(), Marks::default())
    }

    #[test]
    fn an_empty_document_gets_a_paragraph() {
        assert_eq!(normalize(Vec::new()), vec![Block::Paragraph(Vec::new())]);
    }

    #[test]
    fn list_items_start_with_a_paragraph() {
        let fixed = normalize(vec![Block::Bullets(vec![vec![Block::Rule], Vec::new()])]);
        assert_eq!(
            fixed,
            vec![Block::Bullets(vec![
                vec![Block::Paragraph(Vec::new()), Block::Rule],
                vec![Block::Paragraph(Vec::new())],
            ])]
        );
        // A list with no items goes.
        assert_eq!(
            normalize(vec![Block::Numbers(1, Vec::new()), Block::Rule]),
            vec![Block::Rule]
        );
    }

    #[test]
    fn runs_with_the_same_marks_join() {
        let bold = Marks {
            bold: true,
            ..Marks::default()
        };
        assert_eq!(
            normalize_inlines(vec![
                text("a"),
                text(""),
                text("b"),
                Inline::Text("c".into(), bold.clone()),
            ]),
            vec![text("ab"), Inline::Text("c".into(), bold)]
        );
    }

    #[test]
    fn cells_keep_widths_only_when_they_fit() {
        let fixed = normalize_block(Block::Table(vec![vec![Cell {
            header: false,
            colspan: 2,
            rowspan: 0,
            colwidth: Some(vec![100]),
            content: Vec::new(),
        }]]))
        .unwrap();
        let Block::Table(rows) = fixed else { panic!() };
        assert_eq!(rows[0][0].colwidth, None);
        assert_eq!(rows[0][0].rowspan, 1);
        assert_eq!(rows[0][0].content, vec![Block::Paragraph(Vec::new())]);
    }

    #[test]
    fn only_web_and_email_links_stay() {
        assert_eq!(
            safe_link(" https://example.com/a "),
            Some("https://example.com/a".into())
        );
        assert_eq!(
            safe_link("mailto:sam@example.com"),
            Some("mailto:sam@example.com".into())
        );
        assert_eq!(safe_link("javascript:alert(1)"), None);
        assert_eq!(safe_link("JavaScript:alert(1)"), None);
        assert_eq!(safe_link("data:text/html,x"), None);
        assert_eq!(safe_link("/relative"), None);
    }

    #[test]
    fn margins_always_leave_room_for_text() {
        let mut page = PageSetup::new(PageSize::A5);
        page.margins = [-5.0, 288.0, f64::NAN, 288.0];
        let page = page.normalized();
        assert_eq!(page.margins[0], 0.0);
        assert_eq!(page.margins[2], DEFAULT_MARGIN);
        let (width, _) = page.points();
        assert!(page.margins[1] + page.margins[3] <= width - MIN_TEXT);
        assert_eq!(page.margins[1], page.margins[3]);
        // Sideways, the same page has room for them.
        let mut wide = PageSetup::new(PageSize::A5);
        wide.landscape = true;
        wide.margins = [72.0, 200.0, 72.0, 200.0];
        assert_eq!(wide.normalized().margins, [72.0, 200.0, 72.0, 200.0]);
        assert_eq!(wide.css_size(), ("210mm", "148mm"));
    }

    #[test]
    fn the_title_is_the_first_words() {
        let blocks = vec![
            Block::Rule,
            Block::Paragraph(vec![text("  ")]),
            Block::Heading(1, vec![text("Trip"), Inline::Break, text("plan")]),
        ];
        assert_eq!(first_text(&blocks), Some("Trip plan".into()));
        assert_eq!(first_text(&[Block::Paragraph(Vec::new())]), None);
    }
}
