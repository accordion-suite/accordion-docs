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

use base64::Engine as _;
use yrs::branch::BranchID;
use yrs::types::Attrs;
use yrs::types::text::YChange;
use yrs::updates::decoder::Decode as _;
use yrs::updates::encoder::Encode as _;
use yrs::{
    Any, Assoc, IndexedSequence, Map, MapRef, Number, Out, ReadTxn, StickyIndex, Text,
    TransactionMut, Xml, XmlElementPrelim, XmlElementRef, XmlFragment, XmlFragmentRef, XmlOut,
    XmlTextPrelim, XmlTextRef,
};

use crate::comments::Thread;
use crate::model::{
    self, Align, Block, Cell, ContentsStyle, Field, Image, Inline, Kind, ListStyle, Marks,
    PageSetup, PageSize, Para, Parts, Script,
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
            .map(|fragment| {
                model::normalize_blocks(read_children(txn, &fragment, &mut Anchors::default()))
            })
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
    model::normalize(read_children(txn, fragment, &mut Anchors::default()))
}

/// The same as [`read`], with [`Inline::CommentStart`] and
/// [`Inline::CommentEnd`] where each of `threads` starts and ends, for Word
/// and OpenDocument files. A thread whose text is gone has neither. The
/// document's offsets must be UTF-16's, as the editor's are.
pub fn read_with_comments<T: ReadTxn>(
    txn: &T,
    fragment: &XmlFragmentRef,
    threads: &[Thread],
) -> Vec<Block> {
    let mut anchors = Anchors::default();
    for thread in threads {
        let ends = [
            (&thread.start, Inline::CommentStart(thread.id.clone())),
            (&thread.end, Inline::CommentEnd(thread.id.clone())),
        ];
        for (anchor, marker) in ends {
            let offset = base64::engine::general_purpose::STANDARD
                .decode(anchor)
                .ok()
                .and_then(|bytes| StickyIndex::decode_v1(&bytes).ok())
                .and_then(|sticky| sticky.get_offset(txn));
            if let Some(offset) = offset {
                anchors
                    .at
                    .entry(offset.branch.id())
                    .or_default()
                    .push((offset.index, marker));
            }
        }
    }
    // Where one thread ends and another starts, the first ends first.
    for markers in anchors.at.values_mut() {
        markers.sort_by_key(|(index, marker)| (*index, matches!(marker, Inline::CommentStart(_))));
    }
    model::normalize(read_children(txn, fragment, &mut anchors))
}

/// Where comment threads start and end while a document is read.
#[derive(Default)]
struct Anchors {
    /// The markers in each Yjs type, by their index in it: a character's in
    /// a text, a child's in an element.
    at: HashMap<BranchID, Vec<(u32, Inline)>>,
    /// Markers between blocks, waiting for the next text.
    pending: Vec<Inline>,
}

impl Anchors {
    /// The markers in `branch`, taken out.
    fn take(&mut self, branch: &yrs::branch::Branch) -> Vec<(u32, Inline)> {
        if self.at.is_empty() {
            return Vec::new();
        }
        self.at.remove(&branch.id()).unwrap_or_default()
    }
}

/// Each child of `fragment` as a block of its own, normalized, or `None` for
/// a child that isn't one. Comparing these finds what changed.
pub fn read_each<T: ReadTxn>(txn: &T, fragment: &XmlFragmentRef) -> Vec<Option<Block>> {
    fragment
        .children(txn)
        .map(|child| match child {
            XmlOut::Element(element) => {
                read_block(txn, &element, &mut Anchors::default()).and_then(model::normalize_block)
            }
            _ => None,
        })
        .collect()
}

fn read_children<T: ReadTxn, F: XmlFragment>(
    txn: &T,
    parent: &F,
    anchors: &mut Anchors,
) -> Vec<Block> {
    // Markers between blocks go with the next text.
    let mut between = anchors.take(parent.as_ref()).into_iter().peekable();
    let mut blocks = Vec::new();
    for (index, child) in parent.children(txn).enumerate() {
        while let Some((_, marker)) = between.next_if(|(at, _)| *at as usize <= index) {
            anchors.pending.push(marker);
        }
        if let XmlOut::Element(element) = child
            && let Some(block) = read_block(txn, &element, anchors)
        {
            blocks.push(block);
        }
    }
    anchors.pending.extend(between.map(|(_, marker)| marker));
    blocks
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

fn read_block<T: ReadTxn>(
    txn: &T,
    element: &XmlElementRef,
    anchors: &mut Anchors,
) -> Option<Block> {
    Some(match element.tag().as_ref() {
        "paragraph" => {
            Block::Paragraph(read_para(txn, element), read_inlines(txn, element, anchors))
        }
        "heading" => {
            let level = number(txn, element, "level").unwrap_or(1).clamp(1, 6) as u8;
            Block::Heading(
                level,
                read_para(txn, element),
                read_inlines(txn, element, anchors),
            )
        }
        "page_break" => Block::PageBreak,
        "table_of_contents" => Block::Contents(
            string(txn, element, "style")
                .and_then(|style| ContentsStyle::from_name(&style))
                .unwrap_or_default(),
        ),
        "blockquote" => Block::Quote(read_children(txn, element, anchors)),
        "code_block" => Block::Code(
            // Comments in code keep to the text around it.
            read_inlines(txn, element, &mut Anchors::default())
                .into_iter()
                .map(|inline| match inline {
                    Inline::Text(text, _) => text,
                    Inline::Break => "\n".to_owned(),
                    Inline::Footnote(_) | Inline::CommentStart(_) | Inline::CommentEnd(_) => {
                        String::new()
                    }
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
        "bullet_list" => Block::Bullets(
            list_style(txn, element),
            items(txn, element, "list_item", anchors),
        ),
        "ordered_list" => {
            let start = number(txn, element, "order")
                .unwrap_or(1)
                .clamp(0, i64::from(u32::MAX));
            Block::Numbers(
                start as u32,
                list_style(txn, element),
                items(txn, element, "list_item", anchors),
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
                        Some((checked, read_children(txn, &item, anchors)))
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
                            .filter_map(|cell| read_cell(txn, cell, anchors))
                            .collect(),
                    }),
                    _ => None,
                })
                .collect(),
        ),
        _ => return None,
    })
}

fn items<T: ReadTxn>(
    txn: &T,
    list: &XmlElementRef,
    tag: &str,
    anchors: &mut Anchors,
) -> Vec<Vec<Block>> {
    list.children(txn)
        .filter_map(|child| match child {
            XmlOut::Element(item) if item.tag().as_ref() == tag => {
                Some(read_children(txn, &item, anchors))
            }
            _ => None,
        })
        .collect()
}

/// A cell's border attributes: top, right, bottom, and left, in points.
const BORDERS: [&str; 4] = ["border_top", "border_right", "border_bottom", "border_left"];

fn read_cell<T: ReadTxn>(txn: &T, cell: XmlOut, anchors: &mut Anchors) -> Option<Cell> {
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
        content: read_children(txn, &cell, anchors),
    })
}

fn read_inlines<T: ReadTxn>(
    txn: &T,
    element: &XmlElementRef,
    anchors: &mut Anchors,
) -> Vec<Inline> {
    let mut inlines = std::mem::take(&mut anchors.pending);
    let mut between = anchors.take(element.as_ref()).into_iter().peekable();
    for (index, child) in element.children(txn).enumerate() {
        while let Some((_, marker)) = between.next_if(|(at, _)| *at as usize <= index) {
            inlines.push(marker);
        }
        match child {
            XmlOut::Text(text) => {
                // Each marker goes before the character at its index, in
                // UTF-16 code units.
                let mut markers = anchors.take(text.as_ref()).into_iter().peekable();
                let mut at = 0;
                for chunk in text.diff(txn, YChange::identity) {
                    let Out::Any(Any::String(chunk_text)) = chunk.insert else {
                        continue;
                    };
                    let marks = marks(chunk.attributes.as_deref());
                    let mut piece = String::new();
                    for c in chunk_text.chars() {
                        if markers.peek().is_some_and(|(index, _)| *index <= at) {
                            if !piece.is_empty() {
                                inlines
                                    .push(Inline::Text(std::mem::take(&mut piece), marks.clone()));
                            }
                            while let Some((_, marker)) = markers.next_if(|(index, _)| *index <= at)
                            {
                                inlines.push(marker);
                            }
                        }
                        piece.push(c);
                        at += c.len_utf16() as u32;
                    }
                    if !piece.is_empty() {
                        inlines.push(Inline::Text(piece, marks));
                    }
                }
                inlines.extend(markers.map(|(_, marker)| marker));
            }
            XmlOut::Element(inline) if inline.tag().as_ref() == "hard_break" => {
                inlines.push(Inline::Break);
            }
            XmlOut::Element(inline) if inline.tag().as_ref() == "footnote" => {
                // Comments are on the document's text, not in its notes.
                let text = read_inlines(txn, &inline, &mut Anchors::default());
                inlines.push(Inline::Footnote(text));
            }
            _ => {}
        }
    }
    inlines.extend(between.map(|(_, marker)| marker));
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
            "font_weight" => {
                marks.weight = match value {
                    Any::Map(attrs) => match attrs.get("weight") {
                        Some(Any::Number(weight)) => weight
                            .as_f64()
                            .filter(|weight| (100.0..=900.0).contains(weight))
                            .map(|weight| weight as u16),
                        _ => None,
                    },
                    _ => None,
                };
            }
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
const MARK_NAMES: [&str; 14] = [
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
    "font_weight",
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
    if let Some(weight) = marks.weight {
        attrs.insert("font_weight".into(), with("weight", int(i64::from(weight))));
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
    insert_all(txn, parent, index, blocks, &mut Vec::new());
}

/// Inserts `blocks` into `fragment`, which is empty, and returns where each
/// [`Inline::CommentStart`] and [`Inline::CommentEnd`] in them is, as the
/// editor keeps a thread's ends: Yjs positions in base64, by the thread's
/// ID. A thread missing either end isn't there.
pub fn insert_with_comments(
    txn: &mut TransactionMut,
    fragment: &XmlFragmentRef,
    blocks: &[Block],
) -> HashMap<String, (String, String)> {
    let mut found = Vec::new();
    insert_all(txn, fragment, 0, blocks, &mut found);
    // Positions are taken once all the text is in, so each refers to the
    // character after it, as the editor's do.
    let mut ends: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    for (marker, spot) in found {
        // At a type's end, the position is the type's own, as Yjs makes
        // it. yrs before 0.28.2 makes none there.
        let sticky = match &spot {
            Spot::Text(text, index) if *index >= text.len(txn) => {
                Some(StickyIndex::from_type(txn, text, Assoc::After))
            }
            Spot::Element(element, index) if *index >= element.len(txn) => {
                Some(StickyIndex::from_type(txn, element, Assoc::After))
            }
            Spot::Text(text, index) => text.sticky_index(txn, *index, Assoc::After),
            Spot::Element(element, index) => element.sticky_index(txn, *index, Assoc::After),
        };
        let Some(sticky) = sticky else {
            continue;
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(sticky.encode_v1());
        match marker {
            Inline::CommentStart(thread) => ends.entry(thread).or_default().0 = Some(encoded),
            Inline::CommentEnd(thread) => ends.entry(thread).or_default().1 = Some(encoded),
            _ => {}
        }
    }
    ends.into_iter()
        .filter_map(|(thread, ends)| match ends {
            (Some(start), Some(end)) => Some((thread, (start, end))),
            _ => None,
        })
        .collect()
}

/// The position at the start of the document, in base64, for a thread
/// whose text isn't marked: the editor shows it beside the text, as one
/// whose text is gone.
pub fn document_start<T: ReadTxn>(txn: &T, fragment: &XmlFragmentRef) -> Option<String> {
    let sticky = fragment.sticky_index(txn, 0, Assoc::After)?;
    Some(base64::engine::general_purpose::STANDARD.encode(sticky.encode_v1()))
}

/// Where a comment marker was while blocks went in: an index in a text, or
/// a child's index in an element.
enum Spot {
    Text(XmlTextRef, u32),
    Element(XmlElementRef, u32),
}

fn insert_all<P: XmlFragment>(
    txn: &mut TransactionMut,
    parent: &P,
    index: u32,
    blocks: &[Block],
    found: &mut Vec<(Inline, Spot)>,
) {
    for (offset, block) in blocks.iter().enumerate() {
        insert_block(txn, parent, index + offset as u32, block, found);
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

fn insert_block<P: XmlFragment>(
    txn: &mut TransactionMut,
    parent: &P,
    index: u32,
    block: &Block,
    found: &mut Vec<(Inline, Spot)>,
) {
    match block {
        Block::Paragraph(para, inlines) => {
            let el = element(txn, parent, index, "paragraph");
            insert_para(txn, &el, para);
            insert_inlines(txn, &el, inlines, found);
        }
        Block::Heading(level, para, inlines) => {
            let el = element(txn, parent, index, "heading");
            el.insert_attribute(txn, "level", int(i64::from(*level)));
            insert_para(txn, &el, para);
            insert_inlines(txn, &el, inlines, found);
        }
        Block::PageBreak => {
            element(txn, parent, index, "page_break");
        }
        Block::Contents(style) => {
            let el = element(txn, parent, index, "table_of_contents");
            el.insert_attribute(txn, "style", Any::from(style.name()));
        }
        Block::Quote(blocks) => {
            let el = element(txn, parent, index, "blockquote");
            insert_all(txn, &el, 0, blocks, found);
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
            insert_items(txn, &el, "list_item", items, found);
        }
        Block::Numbers(start, style, items) => {
            let el = element(txn, parent, index, "ordered_list");
            el.insert_attribute(txn, "order", int(i64::from(*start)));
            if let Some(style) = style {
                el.insert_attribute(txn, "style", Any::from(style.name()));
            }
            insert_items(txn, &el, "list_item", items, found);
        }
        Block::Checks(items) => {
            let el = element(txn, parent, index, "check_list");
            for (at, (checked, blocks)) in items.iter().enumerate() {
                let item = element(txn, &el, at as u32, "check_item");
                item.insert_attribute(txn, "checked", Any::Bool(*checked));
                insert_all(txn, &item, 0, blocks, found);
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
                    insert_all(txn, &cell_el, 0, &cell.content, found);
                }
            }
        }
    }
}

fn insert_items(
    txn: &mut TransactionMut,
    list: &XmlElementRef,
    tag: &str,
    items: &[Vec<Block>],
    found: &mut Vec<(Inline, Spot)>,
) {
    for (at, blocks) in items.iter().enumerate() {
        let item = element(txn, list, at as u32, tag);
        insert_all(txn, &item, 0, blocks, found);
    }
}

/// Text goes in one XML text for each stretch between line breaks, as
/// y-prosemirror makes them.
fn insert_inlines(
    txn: &mut TransactionMut,
    element: &XmlElementRef,
    inlines: &[Inline],
    found: &mut Vec<(Inline, Spot)>,
) {
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
                insert_inlines(txn, &note, text, &mut Vec::new());
                index += 1;
                run = None;
            }
            marker @ (Inline::CommentStart(_) | Inline::CommentEnd(_)) => {
                let spot = match &run {
                    Some(text) => Spot::Text(text.clone(), text.len(txn)),
                    None => Spot::Element(element.clone(), index),
                };
                found.push((marker.clone(), spot));
            }
        }
    }
}
