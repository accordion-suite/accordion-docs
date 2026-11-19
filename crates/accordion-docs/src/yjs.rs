//! Documents in Yjs, laid out as y-prosemirror lays them out, so the editor
//! in the browser and the server read the same thing (decision 30).
//!
//! The document is the XML fragment [`FRAGMENT`]. Each block is an XML
//! element named after its editor node, with the node's attributes as the
//! element's. A run of text is an XML text whose formatting attributes are
//! its marks, each a map of the mark's attributes, such as `{}` for bold and
//! `{href}` for a link. A line break is a `hard_break` element.

use std::collections::HashMap;
use std::sync::Arc;

use yrs::types::Attrs;
use yrs::types::text::YChange;
use yrs::{
    Any, Map, MapRef, Number, Out, ReadTxn, Text, TransactionMut, Xml, XmlElementPrelim,
    XmlElementRef, XmlFragment, XmlFragmentRef, XmlOut, XmlTextPrelim, XmlTextRef,
};

use crate::model::{self, Block, Cell, Image, Inline, Marks, PageSetup, PageSize};

/// The fragment y-prosemirror binds the editor to.
pub const FRAGMENT: &str = "prosemirror";

/// The map that holds the page setup, beside the fragment. Each part is a
/// key of its own, so one person changing the size and another the margins
/// both keep their change.
pub const PAGE: &str = "page";

const MARGIN_KEYS: [&str; 4] = ["top", "right", "bottom", "left"];

/// The page setup in `page`, or `None` if nobody has set one.
pub fn read_page<T: ReadTxn>(txn: &T, page: &MapRef) -> Option<PageSetup> {
    let size = match page.get(txn, "size")? {
        Out::Any(Any::String(name)) => PageSize::from_name(&name)?,
        _ => return None,
    };
    let landscape = matches!(
        page.get(txn, "orientation"),
        Some(Out::Any(Any::String(orientation))) if &*orientation == "landscape"
    );
    let margins = MARGIN_KEYS.map(|key| match page.get(txn, key) {
        Some(Out::Any(Any::Number(value))) => value.as_f64().unwrap_or(model::DEFAULT_MARGIN),
        _ => model::DEFAULT_MARGIN,
    });
    Some(
        PageSetup {
            size,
            landscape,
            margins,
        }
        .normalized(),
    )
}

/// Makes `page` hold `setup`, or nothing, changing only the keys that
/// differ.
pub fn write_page(txn: &mut TransactionMut, page: &MapRef, setup: Option<&PageSetup>) {
    let Some(setup) = setup else {
        page.clear(txn);
        return;
    };
    // Numbers compare by value, since the editor writes whole ones as
    // integers.
    let mut set = |key: &str, value: Any| {
        let same = match (page.get(txn, key), &value) {
            (Some(Out::Any(Any::Number(have))), Any::Number(want)) => {
                have.as_f64() == want.as_f64()
            }
            (Some(Out::Any(have)), want) => have == *want,
            _ => false,
        };
        if !same {
            page.insert(txn, key, value);
        }
    };
    set("size", Any::from(setup.size.name()));
    set(
        "orientation",
        Any::from(if setup.landscape {
            "landscape"
        } else {
            "portrait"
        }),
    );
    for (key, margin) in MARGIN_KEYS.into_iter().zip(setup.margins) {
        set(key, Any::Number(Number::try_i64(margin)));
    }
}

/// The document's blocks, as the editor would show them.
pub fn read<T: ReadTxn>(txn: &T, fragment: &XmlFragmentRef) -> Vec<Block> {
    model::normalize(read_children(txn, fragment))
}

/// Each child of `fragment` as a block of its own, normalized, or `None` for
/// a child that isn't one. Comparing these finds what changed.
pub fn read_each<T: ReadTxn>(txn: &T, fragment: &XmlFragmentRef) -> Vec<Option<Block>> {
    fragment
        .children(txn)
        .map(|child| match child {
            XmlOut::Element(element) => read_block(txn, &element).and_then(model::normalize_block),
            _ => None,
        })
        .collect()
}

fn read_children<T: ReadTxn, F: XmlFragment>(txn: &T, parent: &F) -> Vec<Block> {
    parent
        .children(txn)
        .filter_map(|child| match child {
            XmlOut::Element(element) => read_block(txn, &element),
            _ => None,
        })
        .collect()
}

fn number<T: ReadTxn>(txn: &T, element: &XmlElementRef, name: &str) -> Option<i64> {
    match element.get_attribute(txn, name)? {
        Out::Any(Any::Number(number)) => number.as_i64(),
        Out::Any(Any::String(text)) => text.trim().parse().ok(),
        _ => None,
    }
}

fn read_block<T: ReadTxn>(txn: &T, element: &XmlElementRef) -> Option<Block> {
    Some(match element.tag().as_ref() {
        "paragraph" => Block::Paragraph(read_inlines(txn, element)),
        "heading" => {
            let level = number(txn, element, "level").unwrap_or(1).clamp(1, 6) as u8;
            Block::Heading(level, read_inlines(txn, element))
        }
        "blockquote" => Block::Quote(read_children(txn, element)),
        "code_block" => Block::Code(
            read_inlines(txn, element)
                .into_iter()
                .map(|inline| match inline {
                    Inline::Text(text, _) => text,
                    Inline::Break => "\n".to_owned(),
                })
                .collect(),
        ),
        "horizontal_rule" => Block::Rule,
        "image" => {
            let text = |name| match element.get_attribute(txn, name) {
                Some(Out::Any(Any::String(text))) => text.to_string(),
                _ => String::new(),
            };
            Block::Image(Image {
                hash: text("src"),
                alt: text("alt"),
                width: number(txn, element, "width").and_then(|width| u32::try_from(width).ok()),
            })
        }
        "bullet_list" => Block::Bullets(items(txn, element, "list_item")),
        "ordered_list" => {
            let start = number(txn, element, "order")
                .unwrap_or(1)
                .clamp(0, i64::from(u32::MAX));
            Block::Numbers(start as u32, items(txn, element, "list_item"))
        }
        "check_list" => Block::Checks(
            element
                .children(txn)
                .filter_map(|child| match child {
                    XmlOut::Element(item) if item.tag().as_ref() == "check_item" => {
                        let checked = matches!(
                            item.get_attribute(txn, "checked"),
                            Some(Out::Any(Any::Bool(true)))
                        );
                        Some((checked, read_children(txn, &item)))
                    }
                    _ => None,
                })
                .collect(),
        ),
        "table" => Block::Table(
            element
                .children(txn)
                .filter_map(|child| match child {
                    XmlOut::Element(row) if row.tag().as_ref() == "table_row" => Some(
                        row.children(txn)
                            .filter_map(|cell| read_cell(txn, cell))
                            .collect(),
                    ),
                    _ => None,
                })
                .collect(),
        ),
        _ => return None,
    })
}

fn items<T: ReadTxn>(txn: &T, list: &XmlElementRef, tag: &str) -> Vec<Vec<Block>> {
    list.children(txn)
        .filter_map(|child| match child {
            XmlOut::Element(item) if item.tag().as_ref() == tag => Some(read_children(txn, &item)),
            _ => None,
        })
        .collect()
}

fn read_cell<T: ReadTxn>(txn: &T, cell: XmlOut) -> Option<Cell> {
    let XmlOut::Element(cell) = cell else {
        return None;
    };
    let header = match cell.tag().as_ref() {
        "table_cell" => false,
        "table_header" => true,
        _ => return None,
    };
    let span = |name| {
        number(txn, &cell, name)
            .unwrap_or(1)
            .clamp(1, i64::from(model::MAX_SPAN)) as u32
    };
    let colwidth = match cell.get_attribute(txn, "colwidth") {
        Some(Out::Any(Any::Array(widths))) => widths
            .iter()
            .map(|width| match width {
                Any::Number(number) => number.as_i64().and_then(|width| u32::try_from(width).ok()),
                _ => None,
            })
            .collect(),
        _ => None,
    };
    Some(Cell {
        header,
        colspan: span("colspan"),
        rowspan: span("rowspan"),
        colwidth,
        content: read_children(txn, &cell),
    })
}

fn read_inlines<T: ReadTxn>(txn: &T, element: &XmlElementRef) -> Vec<Inline> {
    let mut inlines = Vec::new();
    for child in element.children(txn) {
        match child {
            XmlOut::Text(text) => {
                for chunk in text.diff(txn, YChange::identity) {
                    if let Out::Any(Any::String(text)) = chunk.insert {
                        inlines.push(Inline::Text(
                            text.to_string(),
                            marks(chunk.attributes.as_deref()),
                        ));
                    }
                }
            }
            XmlOut::Element(inline) if inline.tag().as_ref() == "hard_break" => {
                inlines.push(Inline::Break);
            }
            _ => {}
        }
    }
    inlines
}

/// The marks in a run's formatting attributes. y-prosemirror names a mark
/// that can overlap itself `name--hash`, so the part before `--` is the
/// mark.
fn marks(attributes: Option<&Attrs>) -> Marks {
    let mut marks = Marks::default();
    let Some(attributes) = attributes else {
        return marks;
    };
    for (name, value) in attributes {
        if matches!(value, Any::Null | Any::Undefined) {
            continue;
        }
        let name = name.split("--").next().unwrap_or_default();
        match name {
            "bold" => marks.bold = true,
            "italic" => marks.italic = true,
            "underline" => marks.underline = true,
            "strike" => marks.strike = true,
            "code" => marks.code = true,
            "link" => {
                if let Any::Map(attrs) = value
                    && let Some(Any::String(href)) = attrs.get("href")
                {
                    marks.link = model::safe_link(href);
                }
            }
            _ => {}
        }
    }
    marks
}

/// The formatting attributes for `marks`, as y-prosemirror writes them.
fn attributes(marks: &Marks) -> Attrs {
    let mut attrs: Attrs = HashMap::new();
    let empty = || Any::Map(Arc::new(HashMap::new()));
    for (on, name) in [
        (marks.bold, "bold"),
        (marks.italic, "italic"),
        (marks.underline, "underline"),
        (marks.strike, "strike"),
        (marks.code, "code"),
    ] {
        if on {
            attrs.insert(name.into(), empty());
        }
    }
    if let Some(href) = &marks.link {
        let mut link = HashMap::new();
        link.insert("href".to_owned(), Any::String(href.as_str().into()));
        attrs.insert("link".into(), Any::Map(Arc::new(link)));
    }
    attrs
}

fn int(value: i64) -> Any {
    Any::Number(yrs::any::Number::Int(value))
}

/// Inserts `blocks` into `parent` at `index`.
pub fn insert_blocks<P: XmlFragment>(
    txn: &mut TransactionMut,
    parent: &P,
    index: u32,
    blocks: &[Block],
) {
    for (offset, block) in blocks.iter().enumerate() {
        insert_block(txn, parent, index + offset as u32, block);
    }
}

fn element<P: XmlFragment>(
    txn: &mut TransactionMut,
    parent: &P,
    index: u32,
    tag: &str,
) -> XmlElementRef {
    parent.insert(txn, index, XmlElementPrelim::empty(tag))
}

fn insert_block<P: XmlFragment>(txn: &mut TransactionMut, parent: &P, index: u32, block: &Block) {
    match block {
        Block::Paragraph(inlines) => {
            let el = element(txn, parent, index, "paragraph");
            insert_inlines(txn, &el, inlines);
        }
        Block::Heading(level, inlines) => {
            let el = element(txn, parent, index, "heading");
            el.insert_attribute(txn, "level", int(i64::from(*level)));
            insert_inlines(txn, &el, inlines);
        }
        Block::Quote(blocks) => {
            let el = element(txn, parent, index, "blockquote");
            insert_blocks(txn, &el, 0, blocks);
        }
        Block::Code(text) => {
            let el = element(txn, parent, index, "code_block");
            if !text.is_empty() {
                let run: XmlTextRef = el.insert(txn, 0, XmlTextPrelim::new(""));
                run.insert_with_attributes(txn, 0, text, HashMap::new());
            }
        }
        Block::Rule => {
            element(txn, parent, index, "horizontal_rule");
        }
        Block::Image(image) => {
            // The editor keeps the hash as the node's `src`, and turns it
            // into the picture's address as it shows it.
            let el = element(txn, parent, index, "image");
            el.insert_attribute(txn, "src", Any::from(image.hash.as_str()));
            el.insert_attribute(txn, "alt", Any::from(image.alt.as_str()));
            if let Some(width) = image.width {
                el.insert_attribute(txn, "width", int(i64::from(width)));
            }
        }
        Block::Bullets(items) => {
            let el = element(txn, parent, index, "bullet_list");
            insert_items(txn, &el, "list_item", items);
        }
        Block::Numbers(start, items) => {
            let el = element(txn, parent, index, "ordered_list");
            el.insert_attribute(txn, "order", int(i64::from(*start)));
            insert_items(txn, &el, "list_item", items);
        }
        Block::Checks(items) => {
            let el = element(txn, parent, index, "check_list");
            for (at, (checked, blocks)) in items.iter().enumerate() {
                let item = element(txn, &el, at as u32, "check_item");
                item.insert_attribute(txn, "checked", Any::Bool(*checked));
                insert_blocks(txn, &item, 0, blocks);
            }
        }
        Block::Table(rows) => {
            let el = element(txn, parent, index, "table");
            for (at, cells) in rows.iter().enumerate() {
                let row = element(txn, &el, at as u32, "table_row");
                for (at, cell) in cells.iter().enumerate() {
                    let tag = if cell.header {
                        "table_header"
                    } else {
                        "table_cell"
                    };
                    let cell_el = element(txn, &row, at as u32, tag);
                    cell_el.insert_attribute(txn, "colspan", int(i64::from(cell.colspan)));
                    cell_el.insert_attribute(txn, "rowspan", int(i64::from(cell.rowspan)));
                    if let Some(widths) = &cell.colwidth {
                        let widths: Vec<Any> =
                            widths.iter().map(|width| int(i64::from(*width))).collect();
                        cell_el.insert_attribute(txn, "colwidth", Any::Array(widths.into()));
                    }
                    insert_blocks(txn, &cell_el, 0, &cell.content);
                }
            }
        }
    }
}

fn insert_items(txn: &mut TransactionMut, list: &XmlElementRef, tag: &str, items: &[Vec<Block>]) {
    for (at, blocks) in items.iter().enumerate() {
        let item = element(txn, list, at as u32, tag);
        insert_blocks(txn, &item, 0, blocks);
    }
}

/// Text goes in one XML text for each stretch between line breaks, as
/// y-prosemirror makes them.
fn insert_inlines(txn: &mut TransactionMut, element: &XmlElementRef, inlines: &[Inline]) {
    let mut index = 0;
    let mut run: Option<XmlTextRef> = None;
    for inline in inlines {
        match inline {
            Inline::Text(text, marks) => {
                let current = match &run {
                    Some(current) => current.clone(),
                    None => {
                        let created: XmlTextRef =
                            element.insert(txn, index, XmlTextPrelim::new(""));
                        index += 1;
                        run = Some(created.clone());
                        created
                    }
                };
                let at = current.len(txn);
                // Every run names all its marks, and none of the others, so
                // it doesn't take on the marks of the run before it.
                let mut attrs = attributes(marks);
                for name in ["bold", "italic", "underline", "strike", "code", "link"] {
                    attrs.entry(name.into()).or_insert(Any::Null);
                }
                current.insert_with_attributes(txn, at, text, attrs);
            }
            Inline::Break => {
                element.insert(txn, index, XmlElementPrelim::empty("hard_break"));
                index += 1;
                run = None;
            }
        }
    }
}
