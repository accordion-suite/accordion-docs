//! What the Word and OpenDocument readers share: blocks read one paragraph
//! at a time ([`Flat`]), put back together into lists, quotes, and code
//! ([`group`]), what a paragraph's style makes it ([`Role`]), list labels,
//! and the paper a page is.

use super::Report;
use crate::model::{Block, Inline, ListStyle, PageSetup, PageSize, Twips};
use crate::style::HalfPoints;

/// What kind of list an item is in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum ListKind {
    Bullets(Option<ListStyle>),
    /// Its style, and the item's number.
    Numbers(Option<ListStyle>, u32),
    Checks(bool),
}

/// A list item's place.
#[derive(Clone, Debug)]
pub(super) struct ListRef {
    pub(super) level: u8,
    pub(super) kind: ListKind,
}

/// A block from the file, with where it goes, before lists, quotes, and
/// code are put together.
#[derive(Clone, Debug)]
pub(super) struct Flat {
    pub(super) block: Block,
    pub(super) list: Option<ListRef>,
    /// More of the list item before, such as a picture in its paragraph.
    pub(super) more: bool,
    pub(super) quote: bool,
    pub(super) code: bool,
}

impl Flat {
    pub(super) fn plain(block: Block) -> Self {
        Self {
            block,
            list: None,
            more: false,
            quote: false,
            code: false,
        }
    }
}

/// Puts lists, quotes, and code back together from paragraphs.
pub(super) fn group(flats: Vec<Flat>) -> Vec<Block> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < flats.len() {
        let flat = &flats[at];
        if flat.list.is_some() {
            let end = flats[at..]
                .iter()
                .position(|flat| flat.list.is_none())
                .map_or(flats.len(), |offset| at + offset);
            let items = &flats[at..end];
            let top = items
                .iter()
                .filter_map(|flat| flat.list.as_ref())
                .map(|list| list.level)
                .min()
                .unwrap_or(0);
            let mut position = 0;
            out.extend(lists(items, &mut position, top));
            at = end;
        } else if flat.quote {
            let end = flats[at..]
                .iter()
                .position(|flat| !flat.quote || flat.list.is_some())
                .map_or(flats.len(), |offset| at + offset);
            let inner = flats[at..end]
                .iter()
                .cloned()
                .map(|flat| Flat {
                    quote: false,
                    ..flat
                })
                .collect();
            out.push(Block::Quote(group(inner)));
            at = end;
        } else if flat.code {
            let end = flats[at..]
                .iter()
                .position(|flat| !flat.code)
                .map_or(flats.len(), |offset| at + offset);
            let lines: Vec<String> = flats[at..end]
                .iter()
                .map(|flat| match &flat.block {
                    Block::Paragraph(_, inlines) | Block::Heading(_, _, inlines) => plain(inlines),
                    _ => String::new(),
                })
                .collect();
            out.push(Block::Code(lines.join("\n")));
            at = end;
        } else {
            out.push(flat.block.clone());
            at += 1;
        }
    }
    out
}

pub(super) fn plain(inlines: &[Inline]) -> String {
    let mut text = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(words, _) => text.push_str(words),
            Inline::Break => text.push('\n'),
            Inline::Footnote(_) | Inline::CommentStart(_) | Inline::CommentEnd(_) => {}
        }
    }
    text
}

/// A list being put together.
struct Building {
    kind: ListKind,
    items: Vec<(bool, Vec<Block>)>,
}

impl Building {
    /// Whether an item of `kind` goes on in this list.
    pub(super) fn continues(&self, kind: ListKind) -> bool {
        match (self.kind, kind) {
            (ListKind::Bullets(a), ListKind::Bullets(b)) => a == b,
            (ListKind::Numbers(a, first), ListKind::Numbers(b, next)) => {
                a == b && next == first + self.items.len() as u32
            }
            (ListKind::Checks(_), ListKind::Checks(_)) => true,
            _ => false,
        }
    }

    pub(super) fn block(self) -> Block {
        match self.kind {
            ListKind::Bullets(style) => Block::Bullets(
                style,
                self.items.into_iter().map(|(_, item)| item).collect(),
            ),
            ListKind::Numbers(style, start) => Block::Numbers(
                start,
                style,
                self.items.into_iter().map(|(_, item)| item).collect(),
            ),
            ListKind::Checks(_) => Block::Checks(
                self.items
                    .into_iter()
                    .map(|(checked, mut blocks)| {
                        if checked && let Some(Block::Paragraph(_, inlines)) = blocks.first_mut() {
                            undone(inlines);
                        }
                        (checked, blocks)
                    })
                    .collect(),
            ),
        }
    }
}

/// Text in `blocks` without bold.
pub(super) fn unbold(blocks: &mut [Block]) {
    for block in blocks {
        if let Block::Paragraph(_, inlines) | Block::Heading(_, _, inlines) = block {
            for inline in inlines {
                if let Inline::Text(_, marks) = inline {
                    marks.bold = false;
                }
            }
        }
    }
}

/// A done checklist item's text without the gray strikethrough Word shows
/// it with, which Docs draws itself.
pub(super) fn undone(inlines: &mut [Inline]) {
    for inline in inlines {
        if let Inline::Text(_, marks) = inline {
            marks.strike = false;
            if marks
                .color
                .is_some_and(|color| color.hex_upper() == "5F6368")
            {
                marks.color = None;
            }
        }
    }
}

/// The lists in `items` from `position` on, at `level`, until an item
/// shallower than it. Deeper items become lists inside the item before.
pub(super) fn lists(items: &[Flat], position: &mut usize, level: u8) -> Vec<Block> {
    let mut out = Vec::new();
    let mut building: Option<Building> = None;
    while *position < items.len() {
        let item = &items[*position];
        let list = item.list.as_ref().expect("only list items");
        if list.level < level {
            break;
        }
        if list.level > level {
            let nested = lists(items, position, list.level);
            match building.as_mut().and_then(|b| b.items.last_mut()) {
                Some((_, blocks)) => blocks.extend(nested),
                None => out.extend(nested),
            }
            continue;
        }
        *position += 1;
        if item.more {
            match building.as_mut().and_then(|b| b.items.last_mut()) {
                Some((_, blocks)) => blocks.push(item.block.clone()),
                None => out.push(item.block.clone()),
            }
            continue;
        }
        if building.as_ref().is_some_and(|b| !b.continues(list.kind)) {
            out.push(building.take().expect("a list").block());
        }
        let checked = matches!(list.kind, ListKind::Checks(true));
        building
            .get_or_insert_with(|| Building {
                kind: list.kind,
                items: Vec::new(),
            })
            .items
            .push((checked, vec![item.block.clone()]));
    }
    if let Some(building) = building {
        out.push(building.block());
    }
    out
}

/// What a paragraph is, from its style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    Normal,
    Heading(u8),
    Title,
    Subtitle,
    Quote,
    Code,
}

impl Role {
    /// The space before and after Docs gives it, in twips, as its own
    /// Word styles have it.
    pub(super) fn spacing(self) -> (Twips, Twips) {
        match self {
            Role::Title => (0, 60),
            Role::Subtitle => (0, 320),
            Role::Heading(1) => (400, 120),
            Role::Heading(2) => (360, 120),
            Role::Heading(3) => (320, 80),
            Role::Heading(4) => (280, 80),
            Role::Heading(_) => (240, 80),
            Role::Normal | Role::Quote | Role::Code => (0, 120),
        }
    }

    /// The size and color Docs gives its text, as its own Word styles have
    /// them.
    pub(super) fn text(self) -> (HalfPoints, Option<&'static str>) {
        match self {
            Role::Title => (52, None),
            Role::Subtitle => (30, Some("#666666")),
            Role::Heading(1) => (40, None),
            Role::Heading(2) => (32, None),
            Role::Heading(3) => (28, Some("#434343")),
            Role::Heading(4) => (24, Some("#666666")),
            Role::Heading(_) => (22, Some("#666666")),
            Role::Quote => (22, Some("#444746")),
            Role::Normal | Role::Code => (22, None),
        }
    }
}

/// A list's number in Word's format, such as "iv" for lowerRoman.
pub(super) fn format_number(value: u32, format: &str) -> String {
    match format {
        "lowerLetter" | "upperLetter" => {
            let mut text = String::new();
            let mut n = value.max(1);
            // Word writes a, b, …, z, aa, bb, ….
            let letter = (b'a' + ((n - 1) % 26) as u8) as char;
            n = (n - 1) / 26 + 1;
            for _ in 0..n.min(10) {
                text.push(letter);
            }
            if format == "upperLetter" {
                text.to_uppercase()
            } else {
                text
            }
        }
        "lowerRoman" | "upperRoman" => {
            let roman = roman(value);
            if format == "upperRoman" {
                roman.to_uppercase()
            } else {
                roman
            }
        }
        "decimalZero" if value < 10 => format!("0{value}"),
        _ => value.to_string(),
    }
}

fn roman(mut value: u32) -> String {
    if value == 0 || value >= 4000 {
        return value.to_string();
    }
    let mut out = String::new();
    for (amount, letters) in [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ] {
        while value >= amount {
            out.push_str(letters);
            value -= amount;
        }
    }
    out
}

/// A bulleted list's kind from its bullet, at `level` from zero: a
/// checklist for a box, or bullets with the style the character is.
pub(super) fn bullet_kind(bullet: Option<char>, level: u8) -> ListKind {
    let style = match bullet {
        Some('☐' | '❏' | '□') => return ListKind::Checks(false),
        Some('☑' | '☒' | '✓' | '✔' | '\u{f0fe}' | '\u{f0fd}') => {
            return ListKind::Checks(true);
        }
        Some('◦' | 'o' | '○' | '\u{f06f}') => ListStyle::Circle,
        Some('▪' | '■' | '\u{f0a7}' | '\u{f0a8}' | '\u{f06e}') => ListStyle::Square,
        Some('–' | '-' | '—' | '\u{f02d}') => ListStyle::Dash,
        Some('➢' | '➤' | '►' | '→' | '➔' | '\u{f0d8}' | '\u{f0e0}') => ListStyle::Arrow,
        Some('★' | '☆' | '\u{f0ab}') => ListStyle::Star,
        _ => ListStyle::Disc,
    };
    // The bullets browsers show at each level need no style.
    let usual = [ListStyle::Disc, ListStyle::Circle, ListStyle::Square][level as usize % 3];
    ListKind::Bullets((style != usual).then_some(style))
}

/// The paper Docs has that's nearest a page `width` by `height` points,
/// and whether it's on its side. The report says which paper Docs uses
/// when the page isn't one Docs has.
pub(super) fn paper(width: f64, height: f64, report: &mut Report) -> (PageSize, bool) {
    let (short, long) = if width < height {
        (width, height)
    } else {
        (height, width)
    };
    let distance = |size: PageSize| {
        let (sw, sh) = PageSetup::new(size).points();
        (sw - short).abs() + (sh - long).abs()
    };
    let paper = PageSize::ALL
        .into_iter()
        .min_by(|a, b| distance(*a).total_cmp(&distance(*b)))
        .unwrap_or_default();
    if distance(paper) > 6.0 {
        report.paper = Some(paper);
    }
    (paper, width > height)
}

/// A paragraph's text, broken where a block has to go between, such as a
/// picture or a page break.
pub(super) enum Piece {
    Text(Vec<Inline>),
    Block(Block),
}

/// Moves text that's only comment marks, such as around a picture, into
/// the text after it, or before it if there's none, so it doesn't become a
/// line of its own.
pub(super) fn move_comment_marks(pieces: &mut [Piece]) {
    let only_marks = |inlines: &[Inline]| {
        !inlines.is_empty()
            && inlines
                .iter()
                .all(|inline| matches!(inline, Inline::CommentStart(_) | Inline::CommentEnd(_)))
    };
    let texts = pieces
        .iter()
        .filter(|piece| matches!(piece, Piece::Text(inlines) if !inlines.is_empty()))
        .count();
    let marks_only = pieces
        .iter()
        .filter(|piece| matches!(piece, Piece::Text(inlines) if only_marks(inlines)))
        .count();
    if marks_only == 0 || texts == marks_only {
        return;
    }
    let mut carried: Vec<Inline> = Vec::new();
    for piece in pieces.iter_mut() {
        if let Piece::Text(inlines) = piece {
            if only_marks(inlines) {
                carried.append(inlines);
            } else if !inlines.is_empty() && !carried.is_empty() {
                carried.append(inlines);
                *inlines = std::mem::take(&mut carried);
            }
        }
    }
    if !carried.is_empty()
        && let Some(Piece::Text(inlines)) = pieces
            .iter_mut()
            .rev()
            .find(|piece| matches!(piece, Piece::Text(inlines) if !inlines.is_empty()))
    {
        inlines.append(&mut carried);
    }
}
