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
            Block::Rule => String::new(),
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
