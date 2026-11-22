//! Documents in Yjs, laid out as y-prosemirror lays them out, so the editor
//! in the browser and the server read the same thing (decision 30).
//!
//! The document is the XML fragment [`FRAGMENT`]. Each block is an XML
//! element named after its editor node, with the node's attributes as the
//! element's. A run of text is an XML text whose formatting attributes are
//! its marks, each a map of the mark's attributes, such as `{}` for bold and
//! `{href}` for a link. A line break is a `hard_break` element, and a
//! footnote a `footnote` element with its text inside, as a paragraph has.

use std::collections::HashMap;
use std::sync::Arc;

use yrs::types::Attrs;
use yrs::types::text::YChange;
use yrs::{
    Any, Map, MapRef, Number, Out, ReadTxn, Text, TransactionMut, Xml, XmlElementPrelim,
    XmlElementRef, XmlFragment, XmlFragmentRef, XmlOut, XmlTextPrelim, XmlTextRef,
};

use crate::model::{
    self, Align, Block, Cell, Field, Image, Inline, Kind, ListStyle, Marks, PageSetup, PageSize,
    Para, Parts, Script,
};
use crate::style::{self, Color};

/// The fragment y-prosemirror binds the editor to.
pub const FRAGMENT: &str = "prosemirror";

/// The map that holds the page setup, beside the fragment. Each part is a
/// key of its own, so one person changing the size and another the margins
/// both keep their change.
pub const PAGE: &str = "page";

const MARGIN_KEYS: [&str; 4] = ["top", "right", "bottom", "left"];

/// The page map's key for whether the first page has a header and footer
/// of its own.
const DIFFERENT_FIRST: &str = "different_first";

/// The headers and footers, each in a fragment named as [`Parts::each`]
/// names it, and whether the first page has its own.
pub fn read_parts<T: ReadTxn>(txn: &T, page: &MapRef) -> Parts {
    let part = |name: &str| {
        txn.get_xml_fragment(name)
            .map(|fragment| model::normalize_blocks(read_children(txn, &fragment)))
            .unwrap_or_default()
    };
    Parts {
        header: part("header"),
        footer: part("footer"),
        first_header: part("first_header"),
        first_footer: part("first_footer"),
        different_first: matches!(
            page.get(txn, DIFFERENT_FIRST),
            Some(Out::Any(Any::Bool(true)))
        ),
    }
    .normalized()
}

/// Makes the document's headers and footers `parts`, changing only the
/// ones that differ.
pub fn write_parts(doc: &yrs::Doc, parts: &Parts) {
    use yrs::Transact as _;
    let page = doc.get_or_insert_map(PAGE);
    let fragments = parts
        .each()
        .map(|(name, _)| doc.get_or_insert_xml_fragment(name));
    let mut txn = doc.transact_mut();
    let have = read_parts(&txn, &page);
    if have.different_first != parts.different_first {
        if parts.different_first {
            page.insert(&mut txn, DIFFERENT_FIRST, true);
        } else {
            page.remove(&mut txn, DIFFERENT_FIRST);
        }
    }
    for ((_, wanted), (fragment, (_, had))) in parts
        .each()
        .into_iter()
        .zip(fragments.iter().zip(have.each()))
    {
        if wanted == had {
            continue;
        }
        let length = fragment.len(&txn);
        if length > 0 {
            fragment.remove_range(&mut txn, 0, length);
        }
        insert_blocks(&mut txn, fragment, 0, wanted);
    }
}

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
        // Whether the first page differs stays, as it isn't the paper's.
        for key in ["size", "orientation"].into_iter().chain(MARGIN_KEYS) {
            page.remove(txn, key);
        }
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

fn float<T: ReadTxn>(txn: &T, element: &XmlElementRef, name: &str) -> Option<f64> {
    match element.get_attribute(txn, name)? {
        Out::Any(Any::Number(number)) => number.as_f64(),
        Out::Any(Any::String(text)) => text.trim().parse().ok(),
        _ => None,
    }
    .filter(|value: &f64| value.is_finite())
}

fn string<T: ReadTxn>(txn: &T, element: &XmlElementRef, name: &str) -> Option<String> {
    match element.get_attribute(txn, name)? {
        Out::Any(Any::String(text)) => Some(text.to_string()),
        _ => None,
    }
}

/// A paragraph's or heading's layout, from its attributes, with lengths
/// in points.
fn read_para<T: ReadTxn>(txn: &T, element: &XmlElementRef) -> Para {
    let length = |name| float(txn, element, name).and_then(model::twips);
    Para {
        kind: string(txn, element, "kind").and_then(|kind| Kind::from_name(&kind)),
        align: string(txn, element, "align")
            .and_then(|align| Align::from_name(&align))
            .unwrap_or_default(),
        indent: length("indent").unwrap_or(0),
        indent_right: length("indent_right").unwrap_or(0),
        first_line: length("first_line").unwrap_or(0),
        line: float(txn, element, "line_height")
            .filter(|line| *line > 0.0 && *line <= 100.0)
            .map(|line| (line * 100.0).round() as u16),
        before: length("space_before"),
        after: length("space_after"),
    }
}

fn list_style<T: ReadTxn>(txn: &T, element: &XmlElementRef) -> Option<ListStyle> {
    string(txn, element, "style").and_then(|style| ListStyle::from_name(&style))
}

fn read_block<T: ReadTxn>(txn: &T, element: &XmlElementRef) -> Option<Block> {
    Some(match element.tag().as_ref() {
        "paragraph" => Block::Paragraph(read_para(txn, element), read_inlines(txn, element)),
        "heading" => {
            let level = number(txn, element, "level").unwrap_or(1).clamp(1, 6) as u8;
            Block::Heading(level, read_para(txn, element), read_inlines(txn, element))
        }
        "page_break" => Block::PageBreak,
        "blockquote" => Block::Quote(read_children(txn, element)),
        "code_block" => Block::Code(
            read_inlines(txn, element)
                .into_iter()
                .map(|inline| match inline {
                    Inline::Text(text, _) => text,
                    Inline::Break => "\n".to_owned(),
                    Inline::Footnote(_) => String::new(),
                })
                .collect(),
        ),
        "horizontal_rule" => Block::Rule(model::Rule {
            thickness: float(txn, element, "thickness").and_then(model::twips),
            color: string(txn, element, "color").and_then(|color| Color::from_hex(&color)),
        }),
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
        "bullet_list" => Block::Bullets(list_style(txn, element), items(txn, element, "list_item")),
        "ordered_list" => {
            let start = number(txn, element, "order")
                .unwrap_or(1)
                .clamp(0, i64::from(u32::MAX));
            Block::Numbers(
                start as u32,
                list_style(txn, element),
                items(txn, element, "list_item"),
            )
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
                    XmlOut::Element(row) if row.tag().as_ref() == "table_row" => Some(model::Row {
                        height: float(txn, &row, "height").and_then(model::twips),
                        cells: row
                            .children(txn)
                            .filter_map(|cell| read_cell(txn, cell))
                            .collect(),
                    }),
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

/// A cell's border attributes: top, right, bottom, and left, in points.
const BORDERS: [&str; 4] = ["border_top", "border_right", "border_bottom", "border_left"];

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
    let length = |name| float(txn, &cell, name).and_then(model::twips);
    let padding = match cell.get_attribute(txn, "padding") {
        Some(Out::Any(Any::Array(sides))) if sides.len() == 4 => {
            let sides: Option<Vec<model::Twips>> = sides
                .iter()
                .map(|side| match side {
                    Any::Number(number) => number.as_f64().and_then(model::twips),
                    _ => None,
                })
                .collect();
            sides.and_then(|sides| sides.try_into().ok())
        }
        _ => None,
    };
    let color = |name| string(txn, &cell, name).and_then(|color| Color::from_hex(&color));
    Some(Cell {
        header,
        colspan: span("colspan"),
        rowspan: span("rowspan"),
        colwidth,
        background: color("background"),
        borders: BORDERS.map(length),
        border_color: color("border_color"),
        padding,
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
            XmlOut::Element(inline) if inline.tag().as_ref() == "footnote" => {
                inlines.push(Inline::Footnote(read_inlines(txn, &inline)));
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
            "superscript" => marks.script = Some(Script::Super),
            "subscript" => marks.script = Some(Script::Sub),
            "text_color" => marks.color = attr_text(value, "color").and_then(Color::from_hex),
            "highlight" => marks.highlight = attr_text(value, "color").and_then(Color::from_hex),
            "font_family" => marks.font = attr_text(value, "family").and_then(style::font_name),
            "font_size" => {
                marks.size = match value {
                    Any::Map(attrs) => match attrs.get("size") {
                        Some(Any::Number(points)) => points.as_f64().and_then(style::half_points),
                        _ => None,
                    },
                    _ => None,
                };
            }
            "field" => marks.field = attr_text(value, "kind").and_then(Field::from_name),
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

/// A text attribute of a mark, such as a color's `color`.
fn attr_text<'a>(value: &'a Any, name: &str) -> Option<&'a str> {
    match value {
        Any::Map(attrs) => match attrs.get(name) {
            Some(Any::String(text)) => Some(text),
            _ => None,
        },
        _ => None,
    }
}

/// The marks y-prosemirror writes, so a run that hasn't one says so.
const MARK_NAMES: [&str; 13] = [
    "bold",
    "italic",
    "underline",
    "strike",
    "code",
    "link",
    "superscript",
    "subscript",
    "text_color",
    "highlight",
    "font_size",
    "font_family",
    "field",
];

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
    let with = |name: &str, value: Any| {
        let mut map = HashMap::new();
        map.insert(name.to_owned(), value);
        Any::Map(Arc::new(map))
    };
    if let Some(href) = &marks.link {
        attrs.insert(
            "link".into(),
            with("href", Any::String(href.as_str().into())),
        );
    }
    match marks.script {
        Some(Script::Super) => {
            attrs.insert("superscript".into(), empty());
        }
        Some(Script::Sub) => {
            attrs.insert("subscript".into(), empty());
        }
        None => {}
    }
    if let Some(color) = marks.color {
        attrs.insert("text_color".into(), with("color", Any::from(color.css())));
    }
    if let Some(color) = marks.highlight {
        attrs.insert("highlight".into(), with("color", Any::from(color.css())));
    }
    if let Some(size) = marks.size {
        attrs.insert(
            "font_size".into(),
            with("size", Any::Number(Number::try_i64(style::points(size)))),
        );
    }
    if let Some(font) = &marks.font {
        attrs.insert(
            "font_family".into(),
            with("family", Any::from(font.as_str())),
        );
    }
    if let Some(field) = marks.field {
        attrs.insert("field".into(), with("kind", Any::from(field.name())));
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

/// A length in points, as the editor keeps it: a whole number when it is
/// one.
fn points(twips: model::Twips) -> Any {
    Any::Number(Number::try_i64(model::points(twips)))
}

/// A paragraph's or heading's layout as its attributes. Parts that aren't
/// set are left out, as y-prosemirror leaves out attributes that are null.
fn insert_para(txn: &mut TransactionMut, el: &XmlElementRef, para: &Para) {
    if let Some(kind) = para.kind {
        el.insert_attribute(txn, "kind", Any::from(kind.name()));
    }
    if para.align != Align::Left {
        el.insert_attribute(txn, "align", Any::from(para.align.name()));
    }
    for (name, twips) in [
        ("indent", para.indent),
        ("indent_right", para.indent_right),
        ("first_line", para.first_line),
    ] {
        if twips != 0 {
            el.insert_attribute(txn, name, points(twips));
        }
    }
    if let Some(line) = para.line {
        el.insert_attribute(
            txn,
            "line_height",
            Any::Number(Number::try_i64(f64::from(line) / 100.0)),
        );
    }
    for (name, space) in [("space_before", para.before), ("space_after", para.after)] {
        if let Some(twips) = space {
            el.insert_attribute(txn, name, points(twips));
        }
    }
}

fn insert_block<P: XmlFragment>(txn: &mut TransactionMut, parent: &P, index: u32, block: &Block) {
    match block {
        Block::Paragraph(para, inlines) => {
            let el = element(txn, parent, index, "paragraph");
            insert_para(txn, &el, para);
            insert_inlines(txn, &el, inlines);
        }
        Block::Heading(level, para, inlines) => {
            let el = element(txn, parent, index, "heading");
            el.insert_attribute(txn, "level", int(i64::from(*level)));
            insert_para(txn, &el, para);
            insert_inlines(txn, &el, inlines);
        }
        Block::PageBreak => {
            element(txn, parent, index, "page_break");
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
        Block::Rule(rule) => {
            let el = element(txn, parent, index, "horizontal_rule");
            if let Some(thickness) = rule.thickness {
                el.insert_attribute(txn, "thickness", points(thickness));
            }
            if let Some(color) = rule.color {
                el.insert_attribute(txn, "color", Any::from(color.css()));
            }
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
        Block::Bullets(style, items) => {
            let el = element(txn, parent, index, "bullet_list");
            if let Some(style) = style {
                el.insert_attribute(txn, "style", Any::from(style.name()));
            }
            insert_items(txn, &el, "list_item", items);
        }
        Block::Numbers(start, style, items) => {
            let el = element(txn, parent, index, "ordered_list");
            el.insert_attribute(txn, "order", int(i64::from(*start)));
            if let Some(style) = style {
                el.insert_attribute(txn, "style", Any::from(style.name()));
            }
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
            for (at, row_of) in rows.iter().enumerate() {
                let row = element(txn, &el, at as u32, "table_row");
                if let Some(height) = row_of.height {
                    row.insert_attribute(txn, "height", points(height));
                }
                for (at, cell) in row_of.cells.iter().enumerate() {
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
                    if let Some(color) = cell.background {
                        cell_el.insert_attribute(txn, "background", Any::from(color.css()));
                    }
                    for (name, border) in BORDERS.iter().zip(cell.borders) {
                        if let Some(border) = border {
                            cell_el.insert_attribute(txn, *name, points(border));
                        }
                    }
                    if let Some(color) = cell.border_color {
                        cell_el.insert_attribute(txn, "border_color", Any::from(color.css()));
                    }
                    if let Some(padding) = cell.padding {
                        let sides: Vec<Any> = padding.into_iter().map(points).collect();
                        cell_el.insert_attribute(txn, "padding", Any::Array(sides.into()));
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
                for name in MARK_NAMES {
                    attrs.entry(name.into()).or_insert(Any::Null);
                }
                current.insert_with_attributes(txn, at, text, attrs);
            }
            Inline::Break => {
                element.insert(txn, index, XmlElementPrelim::empty("hard_break"));
                index += 1;
                run = None;
            }
            Inline::Footnote(text) => {
                let note = element.insert(txn, index, XmlElementPrelim::empty("footnote"));
                insert_inlines(txn, &note, text);
                index += 1;
                run = None;
            }
        }
    }
}
