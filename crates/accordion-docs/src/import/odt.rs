//! Reads OpenDocument text files (`.odt`, and `.ott` templates) into Docs'
//! schema, for **Open with Docs** (decision 30). It runs in the sandbox,
//! since the file is untrusted.
//!
//! It keeps what the Word reader keeps: headings, the title and subtitle,
//! text styles and fonts from the file's styles and the text itself,
//! paragraph layout, lists with their bullets and numbers, checklists
//! written as ☐ and ☑, quotes, code, lines, tables with merged cells,
//! borders, colors, and column widths, links, pictures, footnotes, headers
//! and footers with page numbers and counts, a table of contents, page
//! breaks, and the paper and margins. Comments come too, with their
//! replies and whether they're resolved, as LibreOffice writes them, each
//! about the text it was about. Anything else is left out or made plainer,
//! and the [`Report`] says what.
//!
//! An OpenDocument file Docs wrote comes back as it was: Docs' own styles
//! read as Docs' defaults.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{BufReader, Cursor, Read};

use zip::ZipArchive;

use super::flat::{
    self, Flat, ListKind, ListRef, Piece, Role, bullet_kind, format_number, group,
    move_comment_marks, unbold,
};
use super::xml::{self, Element, Node, Ns};
use super::{Converted, FileComment, Lost, Report, file_thread};
use crate::comments::Thread;
use crate::model::{
    self, Align, Block, Cell, ContentsStyle, Field, Image, Inline, Kind, ListStyle, Marks,
    PageSetup, Para, Parts, Row, Rule, Script, Twips,
};
use crate::picture::{Picture, Pictures};
use crate::style::{Color, HalfPoints};

/// The most XML read from one part, so a small file that unpacks to a huge
/// one stops here.
const MAX_PART: u64 = 64 * 1024 * 1024;

/// The largest picture read from the file.
const MAX_PICTURE: u64 = crate::picture::MAX_PICTURE as u64;

/// How deep styles may be based on each other before Docs stops looking.
const MAX_CHAIN: usize = 32;

/// The most columns or rows a repeat in a table makes. Spreadsheets repeat
/// empty cells to the edge of the sheet, which no document needs.
const MAX_REPEAT: usize = 1000;

/// The most table cells read from one file, repeats included, far more
/// than any document has, so a file that repeats cells without end stops
/// here.
const MAX_CELLS: usize = 200_000;

/// Docs' thin gray line, as it writes cell borders and lines.
const USUAL_LINE: Twips = 15;

/// The file's text, pictures, and page, and what didn't come across.
pub(crate) fn read(bytes: &[u8], report: &mut Report) -> Result<Converted, String> {
    let mut zip = ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| "isn't an OpenDocument file, or it's damaged".to_owned())?;
    let content = part(&mut zip, "content.xml")?
        .ok_or("has no content in it, so it may not be an OpenDocument file")?;
    let styles_part = part(&mut zip, "styles.xml")?;
    let manifest = part(&mut zip, "META-INF/manifest.xml")
        .ok()
        .flatten()
        .map(|root| {
            root.elements()
                .filter_map(|entry| {
                    let path = entry.attr(Ns::Other, "full-path")?;
                    let kind = entry.attr(Ns::Other, "media-type")?;
                    Some((path.trim_end_matches('/').to_owned(), kind.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    let text = content
        .child(Ns::Office, "body")
        .and_then(|body| body.child(Ns::Office, "text"))
        .ok_or("isn't a text document, so Docs can't open it")?;

    let mut styles = Styles::default();
    for root in [styles_part.as_ref(), Some(&content)].into_iter().flatten() {
        if let Some(faces) = root.child(Ns::Office, "font-face-decls") {
            styles.read_fonts(faces);
        }
    }
    if let Some(root) = &styles_part {
        if let Some(common) = root.child(Ns::Office, "styles") {
            styles.read(common, Where::Common);
        }
        if let Some(automatic) = root.child(Ns::Office, "automatic-styles") {
            styles.read(automatic, Where::StylesPart);
        }
    }
    if let Some(automatic) = content.child(Ns::Office, "automatic-styles") {
        styles.read(automatic, Where::Content);
    }
    let masters: Vec<Element> = styles_part
        .as_ref()
        .and_then(|root| root.child(Ns::Office, "master-styles"))
        .map(|masters| {
            masters
                .elements()
                .filter(|master| master.is(Ns::Style, "master-page"))
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    let mut reader = Reader {
        zip,
        manifest,
        styles,
        pictures: Pictures::new(),
        media: HashMap::new(),
        counters: HashMap::new(),
        cells_left: MAX_CELLS,
        lists: HashMap::new(),
        last_list: None,
        next_list: 0,
        outline: Vec::new(),
        in_styles_part: false,
        in_note: false,
        first_master: None,
        masters_used: BTreeSet::new(),
        blocks_read: 0,
        fonts: BTreeSet::new(),
        lost: BTreeSet::new(),
        annotations: HashMap::new(),
        ending: HashMap::new(),
        placed: HashSet::new(),
        between: Vec::new(),
    };
    let threads = reader.read_annotations(text);
    if text
        .child(Ns::Text, "tracked-changes")
        .is_some_and(|changes| changes.elements().next().is_some())
    {
        reader.lost.insert(Lost::TrackedChanges);
    }
    let flats = reader.container(&text.children, &Context::default());
    let blocks = group(flats);

    // The first page's master page: its paper, margins, headers, and
    // footers.
    let first = reader
        .first_master
        .clone()
        .unwrap_or_else(|| "Standard".into());
    let master = masters
        .iter()
        .find(|master| master.attr(Ns::Style, "name") == Some(first.as_str()))
        .or_else(|| masters.first());
    let mut setup = None;
    let mut parts = Parts::default();
    if let Some(master) = master {
        let layout = master
            .attr(Ns::Style, "page-layout-name")
            .and_then(|name| reader.styles.layouts.get(name))
            .cloned();
        // A first page with a master page of its own, as LibreOffice's
        // "First Page", leads on to the master page for the rest.
        let next = master
            .attr(Ns::Style, "next-style-name")
            .filter(|next| Some(*next) != master.attr(Ns::Style, "name"))
            .and_then(|next| {
                masters
                    .iter()
                    .find(|other| other.attr(Ns::Style, "name") == Some(next))
            });
        reader.in_styles_part = true;
        match next {
            Some(rest) => {
                parts.different_first = true;
                parts.first_header = reader.margin_part(master, "header");
                parts.first_footer = reader.margin_part(master, "footer");
                parts.header = reader.margin_part(rest, "header");
                parts.footer = reader.margin_part(rest, "footer");
                reader.even_pages(rest);
            }
            None => {
                parts.header = reader.margin_part(master, "header");
                parts.footer = reader.margin_part(master, "footer");
                let first_header = shown(master, "header-first");
                let first_footer = shown(master, "footer-first");
                if first_header.is_some() || first_footer.is_some() {
                    parts.different_first = true;
                    parts.first_header = reader.margin_part(master, "header-first");
                    parts.first_footer = reader.margin_part(master, "footer-first");
                }
            }
        }
        reader.even_pages(master);
        reader.in_styles_part = false;
        if let Some(layout) = layout {
            let header =
                shown(master, "header").is_some() || shown(master, "header-first").is_some();
            let footer =
                shown(master, "footer").is_some() || shown(master, "footer-first").is_some();
            if layout.columns > 1 {
                reader.lost.insert(Lost::Columns);
            }
            setup = Some(layout.setup(header, footer, report));
        }
        // Pages with another master page and other paper or margins.
        let first_layout = master.attr(Ns::Style, "page-layout-name");
        let differs = reader.masters_used.iter().any(|name| {
            masters
                .iter()
                .find(|other| other.attr(Ns::Style, "name") == Some(name.as_str()))
                .and_then(|other| other.attr(Ns::Style, "page-layout-name"))
                .is_some_and(|layout| {
                    Some(layout) != first_layout
                        && reader
                            .styles
                            .layouts
                            .get(layout)
                            .zip(first_layout.and_then(|first| reader.styles.layouts.get(first)))
                            .is_some_and(|(a, b)| {
                                a.setup(false, false, &mut Report::default())
                                    != b.setup(false, false, &mut Report::default())
                            })
                })
        });
        if differs {
            reader.lost.insert(Lost::Sections);
        }
    }

    for name in &reader.fonts {
        if let Some(replaced) = crate::fonts::replacement(name)
            && !report.fonts.iter().any(|known| known.from == replaced.from)
        {
            report.fonts.push(replaced);
        }
    }
    report.lost.extend(reader.lost);
    Ok(Converted {
        blocks,
        parts,
        setup,
        pictures: reader.pictures,
        comments: threads,
    })
}

/// An annotation's text, a line for each paragraph.
fn annotation_text(annotation: &Element) -> String {
    fn walk(element: &Element, out: &mut String) {
        for node in &element.children {
            match node {
                Node::Text(words) => out.push_str(words),
                Node::Element(child) => match (child.ns, child.name.as_str()) {
                    (Some(Ns::Text), "s") => {
                        let count = child
                            .attr(Ns::Text, "c")
                            .and_then(|count| count.trim().parse::<usize>().ok())
                            .unwrap_or(1)
                            .clamp(1, 1000);
                        out.push_str(&" ".repeat(count));
                    }
                    (Some(Ns::Text), "tab") => out.push(' '),
                    (Some(Ns::Text), "line-break") => out.push('\n'),
                    (Some(Ns::Text), "note") => {}
                    _ => walk(child, out),
                },
            }
        }
    }
    let lines: Vec<String> = annotation
        .descendants(Ns::Text, "p")
        .into_iter()
        .map(|paragraph| {
            let mut line = String::new();
            walk(paragraph, &mut line);
            line
        })
        .collect();
    lines.join("\n")
}

/// The part at `path` as XML, or `None` if the file hasn't one.
fn part(zip: &mut ZipArchive<Cursor<&[u8]>>, path: &str) -> Result<Option<Element>, String> {
    let entry = match zip.by_name(path) {
        Ok(entry) => entry,
        Err(_) => return Ok(None),
    };
    xml::parse(BufReader::new(entry.take(MAX_PART)))
        .map(Some)
        .map_err(|problem| format!("{path} {problem}"))
}

/// A master page's header or footer, unless it's turned off.
fn shown<'a>(master: &'a Element, name: &str) -> Option<&'a Element> {
    master
        .child(Ns::Style, name)
        .filter(|part| part.attr(Ns::Style, "display") != Some("false"))
}

/// A length in points, from OpenDocument's units, such as "2.54cm".
fn length(value: &str) -> Option<f64> {
    let value = value.trim();
    let split = value
        .find(|c: char| c.is_ascii_alphabetic() || c == '%')
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let number: f64 = number.trim().parse().ok()?;
    let points = match unit.trim() {
        "pt" | "" => number,
        "in" => number * 72.0,
        "cm" => number * 72.0 / 2.54,
        "mm" => number * 72.0 / 25.4,
        "pc" => number * 12.0,
        "px" => number * 0.75,
        _ => return None,
    };
    points.is_finite().then_some(points)
}

/// A percentage, such as "115%".
fn percent(value: &str) -> Option<f64> {
    value
        .trim()
        .strip_suffix('%')?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

fn twips_of(value: Option<&str>) -> Option<Twips> {
    model::twips(length(value?)?)
}

/// A color OpenDocument gives as "#rrggbb", unless it's "transparent".
fn color(value: &str) -> Option<Color> {
    Color::from_hex(value)
}

/// A border, such as "0.75pt solid #c4c7c5": its thickness, zero for none,
/// and its color.
fn border(value: &str) -> (Twips, Option<Color>) {
    let mut thickness = None;
    let mut line = true;
    let mut found = None;
    for word in value.split_whitespace() {
        match word {
            "none" | "hidden" => line = false,
            "thin" => thickness = Some(0.75),
            "medium" => thickness = Some(1.5),
            "thick" => thickness = Some(2.25),
            word if word.starts_with('#') => found = color(word),
            word => {
                if let Some(points) = length(word) {
                    thickness = Some(points);
                }
            }
        }
    }
    let thickness = if line {
        thickness
            .and_then(model::twips)
            .unwrap_or(USUAL_LINE)
            .clamp(0, model::MAX_LINE)
    } else {
        0
    };
    (thickness, found)
}

/// A font family as OpenDocument writes one, without quotes, and only the
/// first of a list.
fn family(value: &str) -> Option<String> {
    let first = value
        .split(',')
        .next()?
        .trim()
        .trim_matches(['\'', '"'])
        .trim();
    (!first.is_empty()).then(|| first.to_owned())
}

/// A style's name as people see it, such as "Heading 1" for
/// "Heading_20_1", in lowercase.
fn shown_name(name: &str) -> String {
    let mut out = String::new();
    let mut rest = name;
    while let Some(start) = rest.find('_') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        // `_20_` is a space, and other `_xx_` other characters.
        match after.find('_').and_then(|end| {
            let code = u32::from_str_radix(&after[..end], 16).ok()?;
            Some((char::from_u32(code)?, end))
        }) {
            Some((character, end)) if end > 0 && end <= 4 => {
                out.push(character);
                rest = &after[end + 1..];
            }
            _ => {
                out.push('_');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.to_lowercase()
}

/// A text size: points, or a share of the size it's in.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Size {
    Points(f64),
    Percent(f64),
}

/// Text's look, as a style or the text sets it. Each part is set only
/// where something said so, so later ones override earlier ones.
#[derive(Clone, Debug, Default, PartialEq)]
struct TextProps {
    weight: Option<u16>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    caps: Option<bool>,
    hidden: Option<bool>,
    script: Option<Option<Script>>,
    color: Option<Option<Color>>,
    highlight: Option<Option<Color>>,
    size: Option<Size>,
    font: Option<String>,
}

impl TextProps {
    fn read(props: Option<&Element>, fonts: &HashMap<String, String>) -> Self {
        let mut out = Self::default();
        let Some(props) = props else {
            return out;
        };
        if let Some(weight) = props.attr(Ns::Fo, "font-weight") {
            out.weight = match weight.trim() {
                "bold" => Some(700),
                "normal" => Some(400),
                number => number.parse::<u16>().ok(),
            };
        }
        if let Some(style) = props.attr(Ns::Fo, "font-style") {
            out.italic = Some(matches!(style.trim(), "italic" | "oblique"));
        }
        let on = |value: Option<&str>| value.map(|value| !matches!(value.trim(), "none" | ""));
        out.underline = on(props.attr(Ns::Style, "text-underline-style"))
            .or_else(|| on(props.attr(Ns::Style, "text-underline-type")));
        out.strike = on(props.attr(Ns::Style, "text-line-through-style"))
            .or_else(|| on(props.attr(Ns::Style, "text-line-through-type")));
        if let Some(transform) = props.attr(Ns::Fo, "text-transform") {
            out.caps = Some(transform.trim() == "uppercase");
        }
        if let Some(display) = props.attr(Ns::Text, "display") {
            out.hidden = Some(display.trim() == "none");
        }
        if let Some(position) = props.attr(Ns::Style, "text-position") {
            let first = position.split_whitespace().next().unwrap_or_default();
            out.script = Some(match first {
                "super" => Some(Script::Super),
                "sub" => Some(Script::Sub),
                other => match percent(other) {
                    Some(value) if value > 0.0 => Some(Script::Super),
                    Some(value) if value < 0.0 => Some(Script::Sub),
                    _ => None,
                },
            });
        }
        if props.attr(Ns::Style, "use-window-font-color") == Some("true") {
            out.color = Some(None);
        } else if let Some(value) = props.attr(Ns::Fo, "color") {
            out.color = Some(color(value));
        }
        if let Some(value) = props.attr(Ns::Fo, "background-color") {
            out.highlight = Some(color(value));
        }
        if let Some(size) = props.attr(Ns::Fo, "font-size") {
            out.size = percent(size)
                .map(Size::Percent)
                .or_else(|| length(size).filter(|size| *size > 0.0).map(Size::Points));
        }
        let named = props
            .attr(Ns::Style, "font-name")
            .map(|name| fonts.get(name).cloned().unwrap_or_else(|| name.to_owned()));
        if let Some(font) = named.or_else(|| props.attr(Ns::Fo, "font-family").and_then(family)) {
            out.font = Some(font);
        }
        out
    }

    /// `self` with `other`'s settings on top.
    fn over(&mut self, other: &TextProps) {
        macro_rules! take {
            ($($field:ident),*) => {$(
                if other.$field.is_some() {
                    self.$field = other.$field.clone();
                }
            )*};
        }
        take!(
            weight, italic, underline, strike, caps, hidden, script, color, highlight, font
        );
        match other.size {
            Some(Size::Percent(share)) => {
                self.size = Some(match self.size {
                    Some(Size::Points(points)) => Size::Points(points * share / 100.0),
                    _ => Size::Percent(share),
                });
            }
            Some(size) => self.size = Some(size),
            None => {}
        }
    }

    /// The size in half points, if it's known.
    fn half_points(&self) -> Option<HalfPoints> {
        match self.size {
            Some(Size::Points(points)) => crate::style::half_points(points),
            _ => None,
        }
    }
}

/// How far apart a paragraph's lines are.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Spacing {
    Percent(f64),
    /// Points from line to line, exactly or at least.
    Points(f64),
}

/// A paragraph's layout, as a style or the paragraph sets it.
#[derive(Clone, Debug, Default, PartialEq)]
struct ParaProps {
    align: Option<Align>,
    left: Option<Twips>,
    right: Option<Twips>,
    first: Option<Twips>,
    line: Option<Spacing>,
    before: Option<Twips>,
    after: Option<Twips>,
    break_before: Option<bool>,
    break_after: Option<bool>,
    /// Its bottom border, and whether it has others.
    bottom: Option<(Twips, Option<Color>)>,
    other_borders: Option<bool>,
}

impl ParaProps {
    fn read(props: Option<&Element>) -> Self {
        let mut out = Self::default();
        let Some(props) = props else {
            return out;
        };
        if let Some(align) = props.attr(Ns::Fo, "text-align") {
            out.align = Align::from_name(align);
        }
        let margin = twips_of(props.attr(Ns::Fo, "margin"));
        out.left = twips_of(props.attr(Ns::Fo, "margin-left")).or(margin);
        out.right = twips_of(props.attr(Ns::Fo, "margin-right")).or(margin);
        out.before = twips_of(props.attr(Ns::Fo, "margin-top")).or(margin);
        out.after = twips_of(props.attr(Ns::Fo, "margin-bottom")).or(margin);
        out.first = twips_of(props.attr(Ns::Fo, "text-indent"));
        if let Some(line) = props.attr(Ns::Fo, "line-height") {
            out.line = match line.trim() {
                "normal" => Some(Spacing::Percent(100.0)),
                line => percent(line)
                    .map(Spacing::Percent)
                    .or_else(|| length(line).map(Spacing::Points)),
            };
        } else if let Some(least) = props.attr(Ns::Style, "line-height-at-least") {
            out.line = length(least).map(Spacing::Points);
        }
        if let Some(value) = props.attr(Ns::Fo, "break-before") {
            out.break_before = Some(value.trim() == "page");
        }
        if let Some(value) = props.attr(Ns::Fo, "break-after") {
            out.break_after = Some(value.trim() == "page");
        }
        let all = props.attr(Ns::Fo, "border").map(border);
        let side = |name: &str| props.attr(Ns::Fo, name).map(border).or(all);
        if let Some(bottom) = side("border-bottom") {
            out.bottom = Some(bottom);
        }
        if ["border-top", "border-left", "border-right"]
            .iter()
            .any(|name| side(name).is_some())
        {
            out.other_borders = Some(
                ["border-top", "border-left", "border-right"]
                    .iter()
                    .any(|name| side(name).is_some_and(|(thickness, _)| thickness > 0)),
            );
        }
        out
    }

    fn over(&mut self, other: &ParaProps) {
        macro_rules! take {
            ($($field:ident),*) => {$(
                if other.$field.is_some() {
                    self.$field = other.$field.clone();
                }
            )*};
        }
        take!(
            align,
            left,
            right,
            first,
            line,
            before,
            after,
            break_before,
            break_after,
            bottom,
            other_borders
        );
    }
}

/// A table cell's look.
#[derive(Clone, Debug, Default, PartialEq)]
struct CellProps {
    background: Option<Option<Color>>,
    borders: [Option<(Twips, Option<Color>)>; 4],
    padding: [Option<Twips>; 4],
}

impl CellProps {
    fn read(props: Option<&Element>) -> Self {
        let mut out = Self::default();
        let Some(props) = props else {
            return out;
        };
        if let Some(value) = props.attr(Ns::Fo, "background-color") {
            out.background = Some(color(value));
        }
        let all = props.attr(Ns::Fo, "border").map(border);
        let padding = twips_of(props.attr(Ns::Fo, "padding"));
        for (at, side) in model::SIDES.iter().enumerate() {
            out.borders[at] = props
                .attr(Ns::Fo, &format!("border-{side}"))
                .map(border)
                .or(all);
            out.padding[at] = twips_of(props.attr(Ns::Fo, &format!("padding-{side}"))).or(padding);
        }
        out
    }

    fn over(&mut self, other: &CellProps) {
        if other.background.is_some() {
            self.background = other.background;
        }
        for at in 0..4 {
            if other.borders[at].is_some() {
                self.borders[at] = other.borders[at];
            }
            if other.padding[at].is_some() {
                self.padding[at] = other.padding[at];
            }
        }
    }
}

/// A style of any family.
#[derive(Clone, Debug, Default)]
struct Style {
    /// Its name as people see it, in lowercase.
    shown: String,
    parent: Option<String>,
    list_style: Option<String>,
    master: Option<String>,
    para: ParaProps,
    text: TextProps,
    cell: CellProps,
    /// A table column's width, in points.
    column_width: Option<f64>,
    /// A table row's height, in points.
    row_height: Option<f64>,
    /// How many columns a section's text is in.
    columns: u32,
    /// The outline level its paragraphs are headings at.
    outline: Option<u8>,
}

/// One level of a list style.
#[derive(Clone, Debug)]
enum Level {
    Bullet(Option<char>),
    Number {
        /// OpenDocument's format, such as "1", "a", or "I", or none.
        format: String,
        prefix: String,
        suffix: String,
        start: u32,
        /// How many levels' numbers it shows, such as 2 for "1.2".
        shown: u8,
    },
}

/// A page's paper, margins, and room for headers and footers.
#[derive(Clone, Debug, Default)]
struct Layout {
    width: Option<f64>,
    height: Option<f64>,
    landscape: bool,
    margins: [Option<f64>; 4],
    /// The header's height and the space below it, and the footer's and the
    /// space above it, in points.
    header: f64,
    footer: f64,
    columns: u32,
}

impl Layout {
    fn read(layout: &Element) -> Self {
        let mut out = Self::default();
        let props = layout.child(Ns::Style, "page-layout-properties");
        if let Some(props) = props {
            out.width = props.attr(Ns::Fo, "page-width").and_then(length);
            out.height = props.attr(Ns::Fo, "page-height").and_then(length);
            out.landscape = props.attr(Ns::Style, "print-orientation") == Some("landscape");
            let all = props.attr(Ns::Fo, "margin").and_then(length);
            for (at, side) in model::SIDES.iter().enumerate() {
                out.margins[at] = props
                    .attr(Ns::Fo, &format!("margin-{side}"))
                    .and_then(length)
                    .or(all);
            }
            out.columns = columns(props);
        }
        let room = |name: &str, space: &str| {
            layout
                .child(Ns::Style, name)
                .and_then(|style| style.child(Ns::Style, "header-footer-properties"))
                .map_or(0.0, |props| {
                    let height = props
                        .attr(Ns::Fo, "min-height")
                        .or_else(|| props.attr(Ns::Svg, "height"))
                        .and_then(length)
                        .unwrap_or(0.0);
                    let gap = props.attr(Ns::Fo, space).and_then(length).unwrap_or(0.0);
                    (height + gap).max(0.0)
                })
        };
        out.header = room("header-style", "margin-bottom");
        out.footer = room("footer-style", "margin-top");
        out
    }

    /// The page as Docs keeps it. OpenDocument puts headers and footers
    /// inside the margins, so the text starts below the header.
    fn setup(&self, header: bool, footer: bool, report: &mut Report) -> PageSetup {
        let (size, sideways) = match (self.width, self.height) {
            (Some(width), Some(height)) => flat::paper(width, height, report),
            _ => (model::PageSize::Letter, false),
        };
        let margin = |at: usize| self.margins[at].map_or(model::DEFAULT_MARGIN, f64::abs);
        PageSetup {
            size,
            landscape: self.landscape || sideways,
            margins: [
                margin(0) + if header { self.header } else { 0.0 },
                margin(1),
                margin(2) + if footer { self.footer } else { 0.0 },
                margin(3),
            ],
        }
        .normalized()
    }
}

/// How many columns `props`' `style:columns` has.
fn columns(props: &Element) -> u32 {
    props
        .child(Ns::Style, "columns")
        .and_then(|columns| columns.attr(Ns::Fo, "column-count"))
        .and_then(|count| count.trim().parse().ok())
        .unwrap_or(1)
}

/// Where a style was found. Each part's automatic styles are its own.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Where {
    Common,
    /// Automatic styles in `styles.xml`, for headers and footers.
    StylesPart,
    /// Automatic styles in `content.xml`, for the document's text.
    Content,
}

#[derive(Default)]
struct Styles {
    /// Font faces' names and their families.
    fonts: HashMap<String, String>,
    /// Common styles, by family and name.
    common: HashMap<(String, String), Style>,
    styles_part: HashMap<(String, String), Style>,
    content: HashMap<(String, String), Style>,
    /// Each family's default style.
    defaults: HashMap<String, Style>,
    /// List styles by name, each level from 1.
    lists: HashMap<String, HashMap<u8, Level>>,
    /// The outline's numbering, for headings.
    outline: HashMap<u8, Level>,
    layouts: HashMap<String, Layout>,
}

impl Styles {
    fn read_fonts(&mut self, faces: &Element) {
        for face in faces.elements() {
            if let (Some(name), Some(found)) = (
                face.attr(Ns::Style, "name"),
                face.attr(Ns::Svg, "font-family").and_then(family),
            ) {
                self.fonts.insert(name.to_owned(), found);
            }
        }
    }

    fn read(&mut self, styles: &Element, at: Where) {
        for element in styles.elements() {
            match (element.ns, element.name.as_str()) {
                (Some(Ns::Style), "style") => {
                    let (Some(name), Some(family)) = (
                        element.attr(Ns::Style, "name"),
                        element.attr(Ns::Style, "family"),
                    ) else {
                        continue;
                    };
                    let style = self.style(element);
                    let key = (family.to_owned(), name.to_owned());
                    match at {
                        Where::Common => self.common.insert(key, style),
                        Where::StylesPart => self.styles_part.insert(key, style),
                        Where::Content => self.content.insert(key, style),
                    };
                }
                (Some(Ns::Style), "default-style") => {
                    if let Some(family) = element.attr(Ns::Style, "family") {
                        let style = self.style(element);
                        self.defaults.insert(family.to_owned(), style);
                    }
                }
                (Some(Ns::Text), "list-style") => {
                    if let Some(name) = element.attr(Ns::Style, "name") {
                        self.lists.insert(name.to_owned(), levels(element));
                    }
                }
                (Some(Ns::Text), "outline-style") => self.outline = levels(element),
                (Some(Ns::Style), "page-layout") => {
                    if let Some(name) = element.attr(Ns::Style, "name") {
                        self.layouts.insert(name.to_owned(), Layout::read(element));
                    }
                }
                _ => {}
            }
        }
    }

    fn style(&self, element: &Element) -> Style {
        let name = element
            .attr(Ns::Style, "display-name")
            .map(str::to_lowercase)
            .or_else(|| element.attr(Ns::Style, "name").map(shown_name))
            .unwrap_or_default();
        let section = element.child(Ns::Style, "section-properties");
        Style {
            shown: name,
            parent: element
                .attr(Ns::Style, "parent-style-name")
                .map(str::to_owned),
            list_style: element
                .attr(Ns::Style, "list-style-name")
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
            master: element
                .attr(Ns::Style, "master-page-name")
                .map(str::to_owned),
            para: ParaProps::read(element.child(Ns::Style, "paragraph-properties")),
            text: TextProps::read(element.child(Ns::Style, "text-properties"), &self.fonts),
            cell: CellProps::read(element.child(Ns::Style, "table-cell-properties")),
            column_width: element
                .child(Ns::Style, "table-column-properties")
                .and_then(|props| props.attr(Ns::Style, "column-width"))
                .and_then(length),
            row_height: element
                .child(Ns::Style, "table-row-properties")
                .and_then(|props| {
                    props
                        .attr(Ns::Style, "min-row-height")
                        .or_else(|| props.attr(Ns::Style, "row-height"))
                })
                .and_then(length),
            columns: section.map_or(1, columns),
            outline: element
                .attr(Ns::Style, "default-outline-level")
                .and_then(|level| level.trim().parse::<u8>().ok())
                .filter(|level| *level > 0),
        }
    }

    /// A style and those it's based on, the furthest first. The part's own
    /// automatic styles come first, and they're based on common ones.
    fn chain(&self, family: &str, name: Option<&str>, in_styles_part: bool) -> Vec<&Style> {
        let mut out = Vec::new();
        let Some(name) = name else {
            return out;
        };
        let key = (family.to_owned(), name.to_owned());
        let automatic = if in_styles_part {
            &self.styles_part
        } else {
            &self.content
        };
        let mut next = automatic.get(&key).or_else(|| self.common.get(&key));
        while let Some(style) = next {
            if out.len() >= MAX_CHAIN {
                break;
            }
            out.push(style);
            next = style
                .parent
                .as_ref()
                .and_then(|parent| self.common.get(&(family.to_owned(), parent.clone())));
        }
        out.reverse();
        out
    }
}

/// A list style's levels.
fn levels(style: &Element) -> HashMap<u8, Level> {
    let mut out = HashMap::new();
    for level in style.elements() {
        let Some(number) = level
            .attr(Ns::Text, "level")
            .and_then(|number| number.trim().parse::<u8>().ok())
            .filter(|number| (1..=10).contains(number))
        else {
            continue;
        };
        let read = match level.name.as_str() {
            "list-level-style-bullet" => Level::Bullet(
                level
                    .attr(Ns::Text, "bullet-char")
                    .and_then(|bullet| bullet.chars().next()),
            ),
            "list-level-style-image" => Level::Bullet(None),
            "list-level-style-number" | "outline-level-style" => Level::Number {
                format: level.attr(Ns::Style, "num-format").unwrap_or("").to_owned(),
                prefix: level.attr(Ns::Style, "num-prefix").unwrap_or("").to_owned(),
                suffix: level.attr(Ns::Style, "num-suffix").unwrap_or("").to_owned(),
                start: level
                    .attr(Ns::Text, "start-value")
                    .and_then(|start| start.trim().parse().ok())
                    .unwrap_or(1),
                shown: level
                    .attr(Ns::Text, "display-levels")
                    .and_then(|shown| shown.trim().parse().ok())
                    .unwrap_or(1),
            },
            _ => continue,
        };
        out.insert(number, read);
    }
    out
}

/// Word's name for an OpenDocument number format, for [`format_number`].
fn word_format(format: &str) -> &'static str {
    match format {
        "a" => "lowerLetter",
        "A" => "upperLetter",
        "i" => "lowerRoman",
        "I" => "upperRoman",
        _ => "decimal",
    }
}

/// Where blocks are being read.
#[derive(Clone, Copy, Default)]
struct Context {
    /// In a table cell, whose paragraphs have no space around them.
    cell: bool,
}

/// Inline content as it's read, broken where a block has to go between,
/// such as a picture.
#[derive(Default)]
struct Pieces {
    inlines: Vec<Inline>,
    out: Vec<Piece>,
    /// Blocks read from text boxes, which go after the paragraph.
    after: Vec<Flat>,
    /// Whether any text or picture came from the paragraph.
    shown: bool,
    /// Whether the text so far ends where a space from the file would be
    /// left out: at the start of a line, or after a space.
    at_space: bool,
}

impl Pieces {
    fn new() -> Self {
        Self {
            at_space: true,
            ..Self::default()
        }
    }

    fn block(&mut self, block: Block) {
        let inlines = std::mem::take(&mut self.inlines);
        self.out.push(Piece::Text(inlines));
        self.out.push(Piece::Block(block));
        self.shown = true;
    }
}

/// How a run's text is marked, apart from its own style.
#[derive(Clone)]
struct RunContext {
    base: TextProps,
    role: Role,
    link: Option<String>,
}

/// A list being read: which numbering it carries on, and its style.
#[derive(Clone)]
struct ListContext {
    id: usize,
    style: Option<String>,
    /// How deep, from zero.
    level: u8,
}

struct Reader<'a> {
    zip: ZipArchive<Cursor<&'a [u8]>>,
    /// Each part's media type, from the manifest.
    manifest: HashMap<String, String>,
    styles: Styles,
    pictures: Pictures,
    /// Each picture read, and its hash if Docs keeps it.
    media: HashMap<String, Option<String>>,
    /// Each numbering's numbers so far, by level.
    counters: HashMap<usize, Vec<Option<u32>>>,
    /// How many more table cells may be read.
    cells_left: usize,
    /// Lists by their `xml:id`, for lists that carry on from them.
    lists: HashMap<String, usize>,
    last_list: Option<usize>,
    next_list: usize,
    /// The outline's numbers so far, for numbered headings.
    outline: Vec<u32>,
    /// Reading headers and footers, whose automatic styles are in
    /// `styles.xml`.
    in_styles_part: bool,
    /// In a footnote, which has no pictures, footnotes, or page breaks.
    in_note: bool,
    /// The master page of the first paragraph, which sets the page.
    first_master: Option<String>,
    /// Master pages that pages after the first use.
    masters_used: BTreeSet<String>,
    /// How many blocks of the document's own are read, so the first one's
    /// master page sets the page rather than breaking it.
    blocks_read: usize,
    /// Each font the text names, as named.
    fonts: BTreeSet<String>,
    lost: BTreeSet<Lost>,
    /// The thread each annotation starts, by where the element is, or
    /// `None` for a reply, and by the name its end gives.
    annotations: HashMap<usize, Option<String>>,
    ending: HashMap<String, String>,
    /// The threads whose start has been read.
    placed: HashSet<String>,
    /// Comment marks between paragraphs, for the next one.
    between: Vec<Inline>,
}

/// Where an element is, to know it again when it's read.
fn identity(element: &Element) -> usize {
    std::ptr::from_ref(element) as usize
}

impl Reader<'_> {
    /// The document's annotations as threads: replies, named by
    /// `loext:parent-name` as LibreOffice writes them, join their first
    /// comment's thread.
    fn read_annotations(&mut self, text: &Element) -> Vec<Thread> {
        let found = text.descendants(Ns::Office, "annotation");
        let ends: HashSet<&str> = text
            .descendants(Ns::Office, "annotation-end")
            .into_iter()
            .filter_map(|end| end.attr(Ns::Office, "name"))
            .collect();
        let by_name: HashMap<&str, usize> = found
            .iter()
            .enumerate()
            .filter_map(|(index, annotation)| Some((annotation.attr(Ns::Office, "name")?, index)))
            .collect();
        let first_of = |index: usize| {
            let mut at = index;
            for _ in 0..found.len() {
                match found[at]
                    .attr(Ns::Loext, "parent-name")
                    .and_then(|parent| by_name.get(parent))
                {
                    Some(parent) if *parent != at => at = *parent,
                    _ => break,
                }
            }
            at
        };
        let mut grouped: Vec<(usize, Vec<usize>)> = Vec::new();
        for index in 0..found.len() {
            let first = first_of(index);
            match grouped.iter_mut().find(|(top, _)| *top == first) {
                Some((_, members)) => members.push(index),
                None => grouped.push((first, vec![index])),
            }
        }
        let mut threads = Vec::new();
        for (first, mut members) in grouped {
            members.sort_by_key(|member| (*member != first, *member));
            let resolved = members
                .iter()
                .any(|member| found[*member].attr(Ns::Loext, "resolved") == Some("true"));
            let comments = members
                .iter()
                .map(|member| {
                    let annotation = found[*member];
                    let child_text = |ns, name| annotation.child(ns, name).map(Element::text);
                    FileComment {
                        name: child_text(Ns::Dc, "creator").unwrap_or_default(),
                        text: annotation_text(annotation),
                        at: child_text(Ns::Dc, "date"),
                    }
                })
                .collect();
            let thread = file_thread(comments, resolved);
            for member in &members {
                let annotation = found[*member];
                let starts = (*member == first).then(|| thread.id.clone());
                if let (Some(thread), Some(name)) = (&starts, annotation.attr(Ns::Office, "name"))
                    && ends.contains(name)
                {
                    self.ending.insert(name.to_owned(), thread.clone());
                }
                self.annotations.insert(identity(annotation), starts);
            }
            threads.push(thread);
        }
        threads
    }

    /// Where an annotation's thread starts, and, for an annotation without
    /// an end, ends too. Replies' are their thread's.
    fn annotation_mark(&mut self, annotation: &Element, inlines: &mut Vec<Inline>) {
        if self.in_note || self.in_styles_part {
            return;
        }
        let Some(Some(thread)) = self.annotations.get(&identity(annotation)).cloned() else {
            return;
        };
        if !self.placed.insert(thread.clone()) {
            return;
        }
        inlines.push(Inline::CommentStart(thread.clone()));
        let ends_later = annotation
            .attr(Ns::Office, "name")
            .is_some_and(|name| self.ending.contains_key(name));
        if !ends_later {
            inlines.push(Inline::CommentEnd(thread));
        }
    }

    /// Where a named annotation's text ends.
    fn annotation_end(&mut self, end: &Element, inlines: &mut Vec<Inline>) {
        if self.in_note || self.in_styles_part {
            return;
        }
        if let Some(thread) = end
            .attr(Ns::Office, "name")
            .and_then(|name| self.ending.get(name))
        {
            inlines.push(Inline::CommentEnd(thread.clone()));
        }
    }

    /// The blocks among `nodes`: a body, a cell, a header, or a text box.
    fn container(&mut self, nodes: &[Node], context: &Context) -> Vec<Flat> {
        let mut out = Vec::new();
        for node in nodes {
            let Node::Element(element) = node else {
                continue;
            };
            self.block(element, context, &mut out);
        }
        out
    }

    fn block(&mut self, element: &Element, context: &Context, out: &mut Vec<Flat>) {
        match (element.ns, element.name.as_str()) {
            (Some(Ns::Text), "p" | "h") => {
                self.paragraph(element, context, None, out);
            }
            (Some(Ns::Text), "list") => {
                let list = self.start_list(element, None);
                self.list(element, context, &list, out);
            }
            (Some(Ns::Text), "numbered-paragraph") => {
                let level = element
                    .attr(Ns::Text, "level")
                    .and_then(|level| level.trim().parse::<u8>().ok())
                    .unwrap_or(1)
                    .clamp(1, 10);
                let list = ListContext {
                    level: level - 1,
                    ..self.start_list(element, None)
                };
                for child in element.elements() {
                    if child.is(Ns::Text, "p") || child.is(Ns::Text, "h") {
                        self.paragraph(child, context, Some((&list, None)), out);
                    }
                }
            }
            (Some(Ns::Table), "table") => {
                if let Some(table) = self.table(element) {
                    out.push(Flat::plain(table));
                }
            }
            (Some(Ns::Text), "section") => {
                if element.attr(Ns::Text, "display") == Some("none") {
                    return;
                }
                let style = element.attr(Ns::Text, "style-name");
                if self
                    .styles
                    .chain("section", style, self.in_styles_part)
                    .iter()
                    .any(|style| style.columns > 1)
                {
                    self.lost.insert(Lost::Columns);
                }
                for child in element.elements() {
                    self.block(child, context, out);
                }
            }
            (Some(Ns::Text), "table-of-content") => {
                let style = contents_style(element);
                if let Some(body) = element.child(Ns::Text, "index-body") {
                    for title in body
                        .elements()
                        .filter(|child| child.is(Ns::Text, "index-title"))
                    {
                        for child in title.elements() {
                            self.block(child, context, out);
                        }
                    }
                }
                out.push(Flat::plain(Block::Contents(style)));
            }
            (
                Some(Ns::Text),
                "illustration-index" | "table-index" | "object-index" | "user-index"
                | "alphabetical-index" | "bibliography",
            ) => {
                // Other lists the file made, kept as the text they show.
                if let Some(body) = element.child(Ns::Text, "index-body") {
                    for child in body.elements() {
                        if child.is(Ns::Text, "index-title") {
                            for inner in child.elements() {
                                self.block(inner, context, out);
                            }
                        } else {
                            self.block(child, context, out);
                        }
                    }
                }
            }
            (Some(Ns::Draw), _) => {
                // A picture or text box placed on the page.
                let mut pieces = Pieces::new();
                self.draw(element, &mut pieces);
                for piece in pieces.out {
                    if let Piece::Block(block) = piece {
                        out.push(Flat::plain(block));
                    }
                }
                out.extend(pieces.after);
            }
            (Some(Ns::Office), "annotation") => {
                let mut marks = Vec::new();
                self.annotation_mark(element, &mut marks);
                self.between.extend(marks);
            }
            (Some(Ns::Office), "annotation-end") => {
                let mut marks = Vec::new();
                self.annotation_end(element, &mut marks);
                self.between.extend(marks);
            }
            _ => {}
        }
    }

    /// A list's numbering: one of its own, unless it carries on from
    /// another.
    fn start_list(&mut self, list: &Element, parent: Option<&ListContext>) -> ListContext {
        let style = list
            .attr(Ns::Text, "style-name")
            .map(str::to_owned)
            .or_else(|| parent.and_then(|parent| parent.style.clone()));
        if let Some(parent) = parent {
            return ListContext {
                id: parent.id,
                style,
                level: parent.level + 1,
            };
        }
        let carried = list
            .attr(Ns::Text, "continue-list")
            .and_then(|id| self.lists.get(id).copied())
            .or_else(|| {
                (list.attr(Ns::Text, "continue-numbering") == Some("true"))
                    .then_some(self.last_list)
                    .flatten()
            });
        let id = carried.unwrap_or_else(|| {
            self.next_list += 1;
            self.next_list
        });
        if let Some(own) = list.attr(Ns::Xml, "id") {
            self.lists.insert(own.to_owned(), id);
        }
        self.last_list = Some(id);
        ListContext {
            id,
            style,
            level: 0,
        }
    }

    fn list(
        &mut self,
        element: &Element,
        context: &Context,
        list: &ListContext,
        out: &mut Vec<Flat>,
    ) {
        for item in element.elements() {
            let header = item.is(Ns::Text, "list-header");
            if !header && !item.is(Ns::Text, "list-item") {
                continue;
            }
            let start = item
                .attr(Ns::Text, "start-value")
                .and_then(|start| start.trim().parse::<u32>().ok());
            let style = item
                .attr(Ns::Text, "style-override")
                .map(str::to_owned)
                .or_else(|| list.style.clone());
            let mut first = !header;
            for child in item.elements() {
                if child.is(Ns::Text, "list") {
                    let inner = self.start_list(
                        child,
                        Some(&ListContext {
                            style: style.clone(),
                            ..list.clone()
                        }),
                    );
                    self.list(child, context, &inner, out);
                    first = false;
                    continue;
                }
                let before = out.len();
                if (child.is(Ns::Text, "p") || child.is(Ns::Text, "h")) && first {
                    let item_list = ListContext {
                        style: style.clone(),
                        ..list.clone()
                    };
                    self.paragraph(child, context, Some((&item_list, start)), out);
                    first = false;
                } else {
                    self.block(child, context, out);
                    // More of the item before.
                    let level = list.level;
                    for flat in &mut out[before..] {
                        if flat.list.is_none() && !matches!(flat.block, Block::PageBreak) {
                            flat.list = Some(ListRef {
                                level,
                                kind: ListKind::Bullets(None),
                            });
                            flat.more = true;
                        }
                    }
                }
            }
        }
    }

    /// An item's kind, and its number, which goes on the list's counter.
    fn list_kind(&mut self, list: &ListContext, start: Option<u32>) -> Option<ListKind> {
        let levels = list
            .style
            .as_ref()
            .and_then(|name| self.styles.lists.get(name));
        let level = levels.and_then(|levels| levels.get(&(list.level + 1)).cloned());
        match level {
            Some(Level::Bullet(bullet)) => Some(bullet_kind(bullet, list.level)),
            Some(Level::Number {
                format,
                suffix,
                start: first,
                ..
            }) => {
                if format.is_empty() {
                    return None;
                }
                let number = self.count(list.id, list.level, start, first);
                let style = match format.as_str() {
                    "a" => ListStyle::LowerAlpha,
                    "A" => ListStyle::UpperAlpha,
                    "i" => ListStyle::LowerRoman,
                    "I" => ListStyle::UpperRoman,
                    _ if suffix.trim_end().ends_with(')') => ListStyle::DecimalParen,
                    _ => ListStyle::Decimal,
                };
                Some(ListKind::Numbers(
                    (style != ListStyle::Decimal).then_some(style),
                    number,
                ))
            }
            // A list without a style shows bullets.
            None => Some(bullet_kind(None, list.level)),
        }
    }

    /// The next number at `level` of the numbering `id`, which starts deeper
    /// levels again.
    fn count(&mut self, id: usize, level: u8, own: Option<u32>, start: u32) -> u32 {
        let at = usize::from(level);
        let numbers = self.counters.entry(id).or_default();
        if numbers.len() <= at {
            numbers.resize(at + 1, None);
        }
        let number =
            own.unwrap_or_else(|| numbers[at].map_or(start, |last| last.saturating_add(1)));
        numbers[at] = Some(number);
        numbers.truncate(at + 1);
        number
    }

    /// A numbered heading's number, such as "2.1", from the outline's
    /// numbering, or `None` if headings aren't numbered.
    fn heading_label(&mut self, level: u8, heading: &Element) -> Option<String> {
        let Some(Level::Number {
            format,
            prefix,
            suffix,
            start,
            shown,
        }) = self.styles.outline.get(&level).cloned()
        else {
            return None;
        };
        if format.is_empty() || heading.attr(Ns::Text, "is-list-header") == Some("true") {
            return None;
        }
        let at = usize::from(level - 1);
        if self.outline.len() <= at {
            self.outline.resize(at + 1, 0);
        }
        let own = heading
            .attr(Ns::Text, "start-value")
            .and_then(|value| value.trim().parse::<u32>().ok());
        self.outline[at] = own.unwrap_or(if self.outline[at] == 0 {
            start
        } else {
            self.outline[at] + 1
        });
        self.outline.truncate(at + 1);
        let first = (at + 1).saturating_sub(usize::from(shown.max(1)));
        let numbers: Vec<String> = (first..=at)
            .map(|index| {
                let format = match self.styles.outline.get(&(index as u8 + 1)) {
                    Some(Level::Number { format, .. }) => word_format(format),
                    _ => "decimal",
                };
                format_number(self.outline.get(index).copied().unwrap_or(1).max(1), format)
            })
            .collect();
        Some(format!("{prefix}{}{suffix}", numbers.join(".")))
    }

    fn role(&self, chain: &[&Style], heading: Option<u8>) -> Role {
        for style in chain.iter().rev() {
            match style.shown.as_str() {
                "title" => return Role::Title,
                "subtitle" => return Role::Subtitle,
                _ => {}
            }
        }
        if let Some(level) = heading {
            return Role::Heading(level.clamp(1, 6));
        }
        // A paragraph in a heading's style, as LibreOffice writes headings
        // from Word files without an outline level.
        for style in chain.iter().rev() {
            if let Some(level) = style
                .shown
                .strip_prefix("heading ")
                .and_then(|level| level.trim().parse::<u8>().ok())
                .filter(|level| (1..=10).contains(level))
                .or(style.outline)
            {
                return Role::Heading(level.min(6));
            }
        }
        for style in chain.iter().rev() {
            match style.shown.as_str() {
                "quotations" | "quote" | "quotation" | "block quotation" | "intense quote" => {
                    return Role::Quote;
                }
                "preformatted text" | "code" | "code block" | "source code" => return Role::Code,
                "contents heading" => return Role::Normal,
                _ => {}
            }
        }
        Role::Normal
    }

    fn paragraph(
        &mut self,
        p: &Element,
        context: &Context,
        list: Option<(&ListContext, Option<u32>)>,
        out: &mut Vec<Flat>,
    ) {
        let name = p.attr(Ns::Text, "style-name");
        let chain = self.styles.chain("paragraph", name, self.in_styles_part);
        let mut props = ParaProps::default();
        let mut base = TextProps::default();
        if let Some(default) = self.styles.defaults.get("paragraph") {
            props.over(&default.para);
            base.over(&default.text);
        }
        for style in &chain {
            props.over(&style.para);
            base.over(&style.text);
        }
        let heading = p.is(Ns::Text, "h").then(|| {
            p.attr(Ns::Text, "outline-level")
                .and_then(|level| level.trim().parse::<u8>().ok())
                .filter(|level| *level > 0)
                .unwrap_or(1)
        });
        let role = self.role(&chain, heading);
        let master = chain
            .iter()
            .rev()
            .find_map(|style| style.master.clone())
            .filter(|master| !master.is_empty());
        let list_style = chain
            .iter()
            .rev()
            .find_map(|style| style.list_style.clone());
        let first_block = !self.in_styles_part && !self.in_note && self.blocks_read == 0;
        if !self.in_styles_part && !self.in_note {
            self.blocks_read += 1;
        }
        if first_block {
            self.first_master = master.clone();
        } else if let Some(master) = &master
            && !self.in_styles_part
            && !self.in_note
        {
            // A master page of its own starts a new page.
            self.masters_used.insert(master.clone());
            out.push(Flat::plain(Block::PageBreak));
        }
        if props.break_before == Some(true) && !first_block && !self.in_note {
            // LibreOffice makes a Word file's page break the start of the
            // next paragraph, and leaves the paragraph it was in empty
            // before it, which shows nothing at a page's end.
            if out.last().is_some_and(|flat| {
                flat.list.is_none()
                    && !flat.quote
                    && matches!(&flat.block, Block::Paragraph(_, inlines) if inlines.is_empty())
            }) {
                out.pop();
            }
            out.push(Flat::plain(Block::PageBreak));
            // It also takes away the space before the paragraph, which
            // a page's top has none of anyway.
            if props.before == Some(0) {
                props.before = None;
            }
        }

        let mut list_ref = None;
        let mut label = None;
        if let Some((list, start)) = list {
            let list = ListContext {
                style: list.style.clone().or(list_style),
                ..list.clone()
            };
            match self.list_kind(&list, start) {
                Some(kind) if matches!(role, Role::Normal | Role::Quote) => {
                    list_ref = Some(ListRef {
                        level: list.level,
                        kind,
                    });
                }
                // A numbered heading in a list keeps its number as text.
                Some(ListKind::Numbers(style, number)) => {
                    let format = match style {
                        Some(ListStyle::LowerAlpha) => "lowerLetter",
                        Some(ListStyle::UpperAlpha) => "upperLetter",
                        Some(ListStyle::LowerRoman) => "lowerRoman",
                        Some(ListStyle::UpperRoman) => "upperRoman",
                        _ => "decimal",
                    };
                    label = Some(format!("{}.", format_number(number, format)));
                }
                _ => {}
            }
        } else if let Some(level) = heading
            && matches!(role, Role::Heading(_))
        {
            label = self.heading_label(level.min(10), p);
        }

        let run_context = RunContext {
            base,
            role,
            link: None,
        };
        let mut pieces = Pieces::new();
        pieces.inlines = std::mem::take(&mut self.between);
        if let Some(label) = label.filter(|label| !label.is_empty()) {
            pieces
                .inlines
                .push(Inline::Text(format!("{label} "), Marks::default()));
            pieces.at_space = true;
        }
        self.inlines(&p.children, &run_context, &mut pieces);
        let Pieces {
            inlines,
            out: mut pieces_out,
            after,
            shown,
            ..
        } = pieces;
        pieces_out.push(Piece::Text(inlines));
        let empty = !shown
            && pieces_out
                .iter()
                .all(|piece| matches!(piece, Piece::Text(inlines) if inlines.is_empty()));

        // A paragraph that only breaks the page, as Docs writes a page
        // break.
        if empty && props.break_after == Some(true) && !self.in_note {
            out.push(Flat::plain(Block::PageBreak));
            out.extend(after);
            return;
        }
        // An empty paragraph with only a line under it is a line.
        if empty
            && props.other_borders != Some(true)
            && let Some((thickness, line_color)) = props.bottom
            && thickness > 0
        {
            out.push(Flat::plain(Block::Rule(Rule {
                thickness: Some(thickness).filter(|t| *t != USUAL_LINE),
                color: line_color.filter(|color| color.hex_upper() != "C4C7C5"),
            })));
            out.extend(after);
            return;
        }

        let size = run_context.base.half_points().unwrap_or(22);
        let para = self.para(&props, role, list_ref.is_some(), context.cell, size);
        let in_list = list_ref.is_some();
        let quote = role == Role::Quote && !in_list;
        let code = role == Role::Code && !in_list;
        // Comment marks alone go with the text beside them.
        move_comment_marks(&mut pieces_out);
        let has_blocks = pieces_out
            .iter()
            .any(|piece| matches!(piece, Piece::Block(_)));
        let mut kept: Vec<Piece> = pieces_out
            .into_iter()
            .filter(|piece| !matches!(piece, Piece::Text(inlines) if inlines.is_empty()))
            .collect();
        if kept.is_empty() && !has_blocks {
            kept.push(Piece::Text(Vec::new()));
        }
        let has_text = kept.iter().any(|piece| matches!(piece, Piece::Text(_)));
        let mut first = true;
        for piece in kept {
            let block = match piece {
                Piece::Text(inlines) => match role {
                    Role::Heading(level) => Block::Heading(level, para.clone(), inlines),
                    _ => Block::Paragraph(para.clone(), inlines),
                },
                Piece::Block(block) => {
                    if has_text && matches!(block, Block::Image(_)) {
                        self.lost.insert(Lost::PicturesMoved);
                    }
                    block
                }
            };
            out.push(Flat {
                block,
                list: list_ref.clone(),
                more: !first && list_ref.is_some(),
                quote,
                code,
            });
            first = false;
        }
        out.extend(after);
        if props.break_after == Some(true) && !self.in_note {
            out.push(Flat::plain(Block::PageBreak));
        }
    }

    /// A paragraph's layout in Docs' terms, leaving out what Docs gives it
    /// anyway.
    fn para(
        &self,
        props: &ParaProps,
        role: Role,
        in_list: bool,
        in_cell: bool,
        size: HalfPoints,
    ) -> Para {
        let (before, after) = if in_cell && role == Role::Normal {
            (0, 0)
        } else {
            role.spacing()
        };
        let line = match props.line {
            Some(Spacing::Percent(share)) => Some(share.round().clamp(1.0, 10_000.0) as u16),
            Some(Spacing::Points(points)) => {
                let single = f64::from(size) / 2.0 * model::SINGLE;
                Some((points / single * 100.0).round().clamp(1.0, 10_000.0) as u16)
            }
            None => None,
        }
        .filter(|line| *line != 115);
        let quote = role == Role::Quote;
        Para {
            kind: match role {
                Role::Title => Some(Kind::Title),
                Role::Subtitle => Some(Kind::Subtitle),
                _ => None,
            },
            align: props.align.unwrap_or_default(),
            indent: if in_list || quote {
                0
            } else {
                props.left.unwrap_or(0)
            },
            indent_right: if in_list || quote {
                0
            } else {
                props.right.unwrap_or(0)
            },
            first_line: if in_list { 0 } else { props.first.unwrap_or(0) },
            line,
            before: props.before.filter(|space| !in_list && *space != before),
            after: props.after.filter(|space| !in_list && *space != after),
        }
    }

    fn inlines(&mut self, nodes: &[Node], context: &RunContext, pieces: &mut Pieces) {
        for node in nodes {
            match node {
                Node::Text(words) => self.text(words, &context.base, context, pieces, false),
                Node::Element(element) => self.inline(element, context, pieces),
            }
        }
    }

    fn inline(&mut self, element: &Element, context: &RunContext, pieces: &mut Pieces) {
        match (element.ns, element.name.as_str()) {
            (Some(Ns::Text), "span") => {
                let mut base = context.base.clone();
                for style in self.styles.chain(
                    "text",
                    element.attr(Ns::Text, "style-name"),
                    self.in_styles_part,
                ) {
                    base.over(&style.text);
                }
                if base.hidden == Some(true) {
                    return;
                }
                let inner = RunContext {
                    base,
                    ..context.clone()
                };
                self.inlines(&element.children, &inner, pieces);
            }
            (Some(Ns::Text), "a") => {
                let link = element
                    .attr(Ns::Xlink, "href")
                    .and_then(model::safe_link)
                    .or_else(|| context.link.clone());
                let inner = RunContext {
                    link,
                    ..context.clone()
                };
                self.inlines(&element.children, &inner, pieces);
            }
            (Some(Ns::Text), "s") => {
                let count = element
                    .attr(Ns::Text, "c")
                    .and_then(|count| count.trim().parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, 1000);
                self.text(&" ".repeat(count), &context.base, context, pieces, true);
                pieces.at_space = false;
            }
            (Some(Ns::Text), "tab") => {
                self.lost.insert(Lost::Tabs);
                self.text(" ", &context.base, context, pieces, true);
                pieces.at_space = true;
            }
            (Some(Ns::Text), "line-break") => {
                pieces.shown = true;
                pieces.inlines.push(Inline::Break);
                pieces.at_space = true;
            }
            (Some(Ns::Text), "page-number") => self.field(Field::PageNumber, context, pieces),
            (Some(Ns::Text), "page-count") => self.field(Field::PageCount, context, pieces),
            (Some(Ns::Text), "note") => {
                if self.in_note {
                    return;
                }
                if element.attr(Ns::Text, "note-class") == Some("endnote") {
                    self.lost.insert(Lost::Endnotes);
                }
                if let Some(body) = element.child(Ns::Text, "note-body") {
                    let text = self.note(body);
                    pieces.shown = true;
                    pieces.at_space = false;
                    pieces.inlines.push(Inline::Footnote(text));
                }
            }
            (Some(Ns::Office), "annotation") => self.annotation_mark(element, &mut pieces.inlines),
            (Some(Ns::Office), "annotation-end") => {
                self.annotation_end(element, &mut pieces.inlines)
            }
            (Some(Ns::Text), "change" | "change-start") => {
                self.lost.insert(Lost::TrackedChanges);
            }
            (Some(Ns::Text), "ruby") => {
                if let Some(base) = element.child(Ns::Text, "ruby-base") {
                    self.inlines(&base.children, context, pieces);
                }
            }
            (
                Some(Ns::Text),
                "meta" | "meta-field" | "conditional-text" | "sequence" | "sequence-ref"
                | "bookmark-ref" | "reference-ref" | "note-ref" | "chapter" | "bibliography-mark"
                | "toc-mark-start" | "number",
            ) => {
                if element.name != "number" {
                    self.inlines(&element.children, context, pieces);
                }
            }
            (
                Some(Ns::Text),
                "date" | "time" | "author-name" | "author-initials" | "creator" | "file-name"
                | "title" | "subject" | "description" | "keywords" | "creation-date"
                | "creation-time" | "modification-date" | "modification-time" | "print-date"
                | "print-time" | "printed-by" | "editing-duration" | "editing-cycles"
                | "initial-creator" | "word-count" | "character-count" | "paragraph-count"
                | "page-variable-get" | "variable-get" | "variable-set" | "user-field-get"
                | "user-defined" | "expression" | "text-input" | "database-display" | "drop-down"
                | "placeholder" | "template-name" | "sender-firstname" | "sender-lastname"
                | "sender-company" | "sender-email" | "sender-street" | "sender-city"
                | "sender-country",
            ) => {
                self.lost.insert(Lost::Fields);
                self.inlines(&element.children, context, pieces);
            }
            (Some(Ns::Draw), _) => {
                if self.in_note {
                    self.lost.insert(Lost::Pictures);
                } else {
                    self.draw(element, pieces);
                }
            }
            _ => {}
        }
    }

    /// A page number or count, as the stand-in character Docs keeps its
    /// field on.
    fn field(&mut self, field: Field, context: &RunContext, pieces: &mut Pieces) {
        let mut marks = self.marks(&context.base, context, None);
        marks.field = Some(field);
        pieces.shown = true;
        pieces.at_space = false;
        pieces.inlines.push(Inline::Text("#".into(), marks));
    }

    /// A footnote's text, its paragraphs joined by line breaks.
    fn note(&mut self, body: &Element) -> Vec<Inline> {
        let in_note = std::mem::replace(&mut self.in_note, true);
        let mut text = Vec::new();
        for paragraph in body
            .descendants(Ns::Text, "p")
            .into_iter()
            .chain(body.descendants(Ns::Text, "h"))
        {
            let mut base = TextProps::default();
            if let Some(default) = self.styles.defaults.get("paragraph") {
                base.over(&default.text);
            }
            for style in self.styles.chain(
                "paragraph",
                paragraph.attr(Ns::Text, "style-name"),
                self.in_styles_part,
            ) {
                base.over(&style.text);
            }
            let context = RunContext {
                base,
                role: Role::Normal,
                link: None,
            };
            let mut pieces = Pieces::new();
            self.inlines(&paragraph.children, &context, &mut pieces);
            let mut inlines = pieces.inlines;
            if !text.is_empty() {
                text.push(Inline::Break);
            }
            text.append(&mut inlines);
        }
        self.in_note = in_note;
        text
    }

    /// Docs' marks for text with `props`.
    fn marks(&mut self, props: &TextProps, context: &RunContext, text: Option<&str>) -> Marks {
        let (size, color_default) = if self.in_note {
            (20, None)
        } else {
            context.role.text()
        };
        let color_default = color_default.and_then(Color::from_hex);
        // Docs' sixth heading is italic of itself.
        let italic_default = !self.in_note && context.role == Role::Heading(6);
        let (bold, weight) = match props.weight {
            Some(700) => (true, None),
            Some(400) | None => (false, None),
            Some(weight) => (false, Some(weight)),
        };
        let mut marks = Marks {
            bold,
            weight,
            italic: props.italic == Some(true) && !italic_default,
            underline: props.underline == Some(true),
            strike: props.strike == Some(true),
            script: props.script.flatten(),
            color: props
                .color
                .flatten()
                // Near black is the text's own color.
                .filter(|color| color.0.iter().any(|part| *part > 0x20))
                .filter(|color| Some(*color) != color_default),
            highlight: props.highlight.flatten(),
            size: props.half_points().filter(|own| *own != size),
            font: props.font.clone(),
            link: context.link.clone(),
            ..Marks::default()
        };
        if marks.link.is_some() {
            marks.underline = false;
            if marks.color.is_some_and(|color| {
                matches!(
                    color.hex_upper().as_str(),
                    "0563C1" | "1155CC" | "0000FF" | "000080"
                )
            }) {
                marks.color = None;
            }
        }
        if let Some(font) = &props.font
            && text.is_some_and(|text| !text.trim().is_empty())
        {
            self.fonts.insert(font.clone());
        }
        // Docs' own code marks, as it writes them.
        if marks.font.as_deref() == Some(crate::fonts::CODE)
            && marks
                .highlight
                .is_some_and(|color| color.hex_upper() == "F1F3F4")
        {
            marks.code = true;
            marks.font = None;
            marks.highlight = None;
        }
        marks
    }

    /// Text from the file. Unless it's `literal`, its white space is one
    /// space, as OpenDocument reads it, and none at a line's start.
    fn text(
        &mut self,
        words: &str,
        props: &TextProps,
        context: &RunContext,
        pieces: &mut Pieces,
        literal: bool,
    ) {
        if props.hidden == Some(true) {
            return;
        }
        let mut text = String::with_capacity(words.len());
        if literal {
            text.push_str(words);
        } else {
            for c in words.chars() {
                if c.is_whitespace() && !matches!(c, '\u{a0}' | '\u{202f}') {
                    if !pieces.at_space {
                        text.push(' ');
                        pieces.at_space = true;
                    }
                } else {
                    text.push(c);
                    pieces.at_space = false;
                }
            }
        }
        if text.is_empty() {
            return;
        }
        pieces.shown = true;
        let text = if props.caps == Some(true) {
            text.to_uppercase()
        } else {
            text
        };
        let marks = self.marks(props, context, Some(&text));
        // Next to text with the same marks, it's one run.
        if let Some(Inline::Text(last, last_marks)) = pieces.inlines.last_mut()
            && *last_marks == marks
        {
            last.push_str(&text);
            return;
        }
        pieces.inlines.push(Inline::Text(text, marks));
    }

    /// A frame: a picture, a text box, or an object, or a shape.
    fn draw(&mut self, element: &Element, pieces: &mut Pieces) {
        match element.name.as_str() {
            "frame" => {}
            // A link around a frame.
            "a" => {
                for child in element.elements() {
                    if child.ns == Some(Ns::Draw) {
                        self.draw(child, pieces);
                    }
                }
                return;
            }
            _ => {
                self.lost.insert(Lost::Shapes);
                let paragraphs: Vec<&Element> = element
                    .descendants(Ns::Text, "p")
                    .into_iter()
                    .chain(element.descendants(Ns::Text, "h"))
                    .collect();
                if !paragraphs.is_empty() && !self.in_note {
                    self.lost.insert(Lost::TextBoxes);
                    for paragraph in paragraphs {
                        self.paragraph(paragraph, &Context::default(), None, &mut pieces.after);
                    }
                    pieces.shown = true;
                }
                return;
            }
        }
        let width = element
            .attr(Ns::Svg, "width")
            .and_then(length)
            // Points as pixels at 96 to the inch.
            .map(|points| (points / 0.75).round().clamp(1.0, 5000.0) as u32);
        let alt = element
            .child(Ns::Svg, "desc")
            .map(Element::text)
            .filter(|alt| !alt.trim().is_empty())
            .or_else(|| element.child(Ns::Svg, "title").map(Element::text))
            .unwrap_or_default()
            .trim()
            .to_owned();
        let images: Vec<&Element> = element
            .elements()
            .filter(|child| child.is(Ns::Draw, "image"))
            .collect();
        let objects: Vec<&Element> = element
            .elements()
            .filter(|child| child.is(Ns::Draw, "object") || child.is(Ns::Draw, "object-ole"))
            .collect();
        if let Some(text_box) = element.child(Ns::Draw, "text-box") {
            if !self.in_note {
                self.lost.insert(Lost::TextBoxes);
                let flats = self.container(&text_box.children, &Context::default());
                pieces.after.extend(flats);
                pieces.shown = true;
            }
            return;
        }
        if !objects.is_empty() {
            let formula = objects.iter().any(|object| {
                object
                    .attr(Ns::Xlink, "href")
                    .map(|href| href.trim_start_matches("./").trim_end_matches('/'))
                    .and_then(|path| self.manifest.get(path))
                    .is_some_and(|kind| kind.contains("formula"))
            });
            self.lost.insert(if formula {
                Lost::Equations
            } else {
                Lost::Shapes
            });
            return;
        }
        let mut found = None;
        for image in images {
            let Some(href) = image.attr(Ns::Xlink, "href") else {
                continue;
            };
            if href.contains("://") {
                continue;
            }
            if let Some(hash) = self.picture(href.trim_start_matches("./")) {
                found = Some(hash);
                break;
            }
        }
        match found {
            Some(hash) => pieces.block(Block::Image(Image { hash, alt, width })),
            None => {
                self.lost.insert(Lost::Pictures);
            }
        }
    }

    /// The picture at `path` in the file, by its hash, if Docs keeps it.
    fn picture(&mut self, path: &str) -> Option<String> {
        if let Some(known) = self.media.get(path) {
            return known.clone();
        }
        let bytes = self.zip.by_name(path).ok().and_then(|entry| {
            let mut out = Vec::new();
            entry.take(MAX_PICTURE + 1).read_to_end(&mut out).ok()?;
            (out.len() as u64 <= MAX_PICTURE).then_some(out)
        });
        let found = bytes.and_then(Picture::read).map(|picture| {
            let hash = picture.hash.clone();
            self.pictures.insert(hash.clone(), picture);
            hash
        });
        self.media.insert(path.to_owned(), found.clone());
        found
    }

    /// A master page's header or footer, as blocks.
    fn margin_part(&mut self, master: &Element, name: &str) -> Vec<Block> {
        match shown(master, name) {
            Some(part) => {
                let flats = self.container(&part.children, &Context::default());
                group(flats)
            }
            None => Vec::new(),
        }
    }

    /// Notes headers and footers of their own on even pages.
    fn even_pages(&mut self, master: &Element) {
        if shown(master, "header-left").is_some() || shown(master, "footer-left").is_some() {
            self.lost.insert(Lost::EvenPages);
        }
    }

    fn table(&mut self, table: &Element) -> Option<Block> {
        // Each column's width in pixels, if known.
        let mut widths: Vec<Option<u32>> = Vec::new();
        let mut rows: Vec<(&Element, bool)> = Vec::new();
        collect_table(table, false, &mut widths, &mut rows, self, 0);
        if rows.is_empty() {
            return None;
        }
        let mut out = Vec::new();
        for (tr, in_header) in rows {
            let row_style = self.styles.chain(
                "table-row",
                tr.attr(Ns::Table, "style-name"),
                self.in_styles_part,
            );
            let height = row_style
                .iter()
                .rev()
                .find_map(|style| style.row_height)
                .and_then(model::twips)
                .map(|height| height.clamp(0, model::MAX_ROW_HEIGHT))
                .filter(|height| *height > 0);
            let mut cells = Vec::new();
            let mut column = 0usize;
            for cell in tr.elements() {
                let repeated = cell
                    .attr(Ns::Table, "number-columns-repeated")
                    .and_then(|count| count.trim().parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, MAX_REPEAT);
                if cell.is(Ns::Table, "covered-table-cell") {
                    column += repeated;
                    continue;
                }
                if !cell.is(Ns::Table, "table-cell") {
                    continue;
                }
                for _ in 0..repeated {
                    if self.cells_left == 0 || cells.len() >= MAX_REPEAT {
                        break;
                    }
                    self.cells_left -= 1;
                    let made = self.cell(cell, in_header, column, &widths);
                    column += made.colspan as usize;
                    cells.push(made);
                }
            }
            if cells.is_empty() && self.cells_left == 0 {
                break;
            }
            out.push(Row { height, cells });
        }
        Some(Block::Table(out))
    }

    fn cell(
        &mut self,
        cell: &Element,
        in_header: bool,
        column: usize,
        widths: &[Option<u32>],
    ) -> Cell {
        let span = |name: &str| {
            cell.attr(Ns::Table, name)
                .and_then(|span| span.trim().parse::<u32>().ok())
                .unwrap_or(1)
                .clamp(1, model::MAX_SPAN)
        };
        let colspan = span("number-columns-spanned");
        let rowspan = span("number-rows-spanned");
        let mut props = CellProps::default();
        let chain = self.styles.chain(
            "table-cell",
            cell.attr(Ns::Table, "style-name"),
            self.in_styles_part,
        );
        for style in &chain {
            props.over(&style.cell);
        }
        let flats = self.container(&cell.children, &Context { cell: true });
        // A cell is a heading cell in the table's heading rows, or when its
        // text is in the table heading style, as Docs writes heading cells.
        let heading_text = cell
            .elements()
            .filter(|child| child.is(Ns::Text, "p"))
            .any(|p| {
                self.styles
                    .chain(
                        "paragraph",
                        p.attr(Ns::Text, "style-name"),
                        self.in_styles_part,
                    )
                    .iter()
                    .any(|style| style.shown == "table heading")
            });
        let header = in_header || heading_text;
        let mut content = group(flats);
        if header {
            unbold(&mut content);
        }
        let background = props
            .background
            .flatten()
            .filter(|fill| fill.hex_upper() != "FFFFFF")
            .filter(|fill| !(header && fill.hex_upper() == "F1F3F4"));
        let mut borders = [None; 4];
        let mut colors = Vec::new();
        for (at, side) in props.borders.iter().enumerate() {
            borders[at] = match side {
                // Docs' thin gray line, as it writes it here, and as Word
                // files carry it.
                Some((thickness, color))
                    if (*thickness == USUAL_LINE || *thickness == 10)
                        && color.is_some_and(|color| color.hex_upper() == "C4C7C5") =>
                {
                    None
                }
                Some((thickness, color)) => {
                    if *thickness > 0 {
                        colors.push(color.unwrap_or(Color::BLACK));
                    }
                    Some(*thickness)
                }
                None => Some(0),
            };
        }
        let border_color = colors
            .first()
            .copied()
            .filter(|color| *color != Color::BLACK);
        // Docs' usual padding, as it writes it here and in Word files.
        let padding = if props.padding.iter().all(Option::is_none)
            || props.padding == [Some(80); 4]
            || props.padding == [Some(60), Some(108), Some(60), Some(108)]
        {
            None
        } else {
            Some(
                props
                    .padding
                    .map(|side| side.unwrap_or(0).clamp(0, model::MAX_PADDING)),
            )
        };
        let reach = column + colspan as usize;
        let colwidth = (reach <= widths.len()
            && widths[column..reach]
                .iter()
                .all(|width| width.is_some_and(|w| w > 0)))
        .then(|| {
            widths[column..reach]
                .iter()
                .map(|width| width.unwrap_or(0))
                .collect()
        });
        Cell {
            header,
            colspan,
            rowspan,
            colwidth,
            background,
            borders,
            border_color: if borders.iter().any(|side| side.is_some_and(|t| t > 0)) {
                border_color
            } else {
                None
            },
            padding,
            content,
        }
    }
}

/// A table's columns' widths and its rows, inside any groups of either,
/// and whether each row is a heading row.
fn collect_table<'e>(
    element: &'e Element,
    in_header: bool,
    widths: &mut Vec<Option<u32>>,
    rows: &mut Vec<(&'e Element, bool)>,
    reader: &Reader,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    for child in element.elements() {
        match (child.ns, child.name.as_str()) {
            (Some(Ns::Table), "table-column") => {
                let repeated = child
                    .attr(Ns::Table, "number-columns-repeated")
                    .and_then(|count| count.trim().parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, MAX_REPEAT);
                let width = reader
                    .styles
                    .chain(
                        "table-column",
                        child.attr(Ns::Table, "style-name"),
                        reader.in_styles_part,
                    )
                    .iter()
                    .rev()
                    .find_map(|style| style.column_width)
                    .map(|points| (points / 0.75).round().clamp(0.0, 5000.0) as u32);
                for _ in 0..repeated {
                    if widths.len() >= MAX_REPEAT {
                        break;
                    }
                    widths.push(width);
                }
            }
            (Some(Ns::Table), "table-row") => {
                let repeated = child
                    .attr(Ns::Table, "number-rows-repeated")
                    .and_then(|count| count.trim().parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, MAX_REPEAT);
                for _ in 0..repeated {
                    if rows.len() >= MAX_CELLS {
                        break;
                    }
                    rows.push((child, in_header));
                }
            }
            (Some(Ns::Table), "table-header-rows") => {
                collect_table(child, true, widths, rows, reader, depth + 1);
            }
            (
                Some(Ns::Table),
                "table-columns"
                | "table-header-columns"
                | "table-column-group"
                | "table-rows"
                | "table-row-group",
            ) => {
                collect_table(child, in_header, widths, rows, reader, depth + 1);
            }
            _ => {}
        }
    }
}

/// How a table of contents looks, from how its entries are made: with page
/// numbers after dots or spaces, or links alone.
fn contents_style(contents: &Element) -> ContentsStyle {
    let template = contents
        .child(Ns::Text, "table-of-content-source")
        .and_then(|source| {
            source
                .elements()
                .find(|template| template.is(Ns::Text, "table-of-content-entry-template"))
        });
    let Some(template) = template else {
        return ContentsStyle::Plain;
    };
    if template
        .child(Ns::Text, "index-entry-page-number")
        .is_none()
    {
        return ContentsStyle::Links;
    }
    let dotted = template
        .elements()
        .filter(|part| part.is(Ns::Text, "index-entry-tab-stop"))
        .any(|tab| tab.attr(Ns::Style, "leader-char") == Some("."));
    if dotted {
        ContentsStyle::Dotted
    } else {
        ContentsStyle::Plain
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, PageSize};
    use crate::odt;

    fn text(words: &str) -> Inline {
        Inline::Text(words.to_owned(), Marks::default())
    }

    fn para(words: &str) -> Block {
        Block::Paragraph(Para::default(), vec![text(words)])
    }

    fn png(seed: u8) -> Picture {
        let mut bytes =
            b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x02\0\0\0\x90wS\xde".to_vec();
        bytes.extend_from_slice(&[0, 0, 0, 1, seed]);
        Picture::read(bytes).expect("a picture")
    }

    fn read_back(
        blocks: &[Block],
        parts: &Parts,
        setup: &PageSetup,
        pictures: &Pictures,
    ) -> (Converted, Report) {
        let file = odt::write(blocks, parts, Some(setup), "Test", pictures, &[], &[]);
        let mut report = Report::default();
        let mut read = read(&file, &mut report).expect("Docs reads its own OpenDocument files");
        read.blocks = model::normalize(read.blocks);
        read.parts = read.parts.normalized();
        (read, report)
    }

    #[test]
    fn comments_come_back_from_docs_opendocument_files() {
        let (blocks, threads) = super::super::comment_fixture::document();
        let file = odt::write(
            &blocks,
            &Parts::default(),
            None,
            "Test",
            &Pictures::new(),
            &[],
            &threads,
        );
        let mut report = Report::default();
        let mut read = read(&file, &mut report).unwrap();
        read.blocks = model::normalize(read.blocks);
        super::super::comment_fixture::check(&read);
    }

    #[test]
    fn docs_opendocument_files_come_back_the_same() {
        let picture = png(7);
        let mut pictures = Pictures::new();
        pictures.insert(picture.hash.clone(), picture.clone());
        let styled = Marks {
            italic: true,
            underline: true,
            color: Some(Color([0xcc, 0, 0])),
            highlight: Some(Color([0xff, 0xf2, 0xcc])),
            size: Some(28),
            font: Some("Lato".into()),
            weight: Some(300),
            ..Marks::default()
        };
        let blocks = model::normalize(vec![
            Block::Paragraph(
                Para {
                    kind: Some(Kind::Title),
                    ..Para::default()
                },
                vec![text("Plans")],
            ),
            Block::Contents(ContentsStyle::Dotted),
            Block::Heading(1, Para::default(), vec![text("First")]),
            Block::Paragraph(
                Para {
                    align: Align::Center,
                    indent: 720,
                    first_line: -360,
                    line: Some(200),
                    before: Some(240),
                    after: Some(0),
                    ..Para::default()
                },
                vec![
                    text(" Some  "),
                    Inline::Text("styled".into(), styled),
                    text(" and "),
                    Inline::Text(
                        "a link".into(),
                        Marks {
                            link: Some("https://example.com/".into()),
                            ..Marks::default()
                        },
                    ),
                    Inline::Text(
                        "code".into(),
                        Marks {
                            code: true,
                            ..Marks::default()
                        },
                    ),
                    Inline::Text(
                        "up".into(),
                        Marks {
                            script: Some(Script::Super),
                            bold: true,
                            ..Marks::default()
                        },
                    ),
                    Inline::Footnote(vec![text("A note"), Inline::Break, text("on two lines")]),
                ],
            ),
            Block::Bullets(
                None,
                vec![
                    vec![
                        para("One"),
                        Block::Numbers(
                            4,
                            Some(ListStyle::LowerRoman),
                            vec![vec![para("Deep")], vec![para("Deeper")]],
                        ),
                    ],
                    vec![para("Two")],
                ],
            ),
            Block::Bullets(Some(ListStyle::Star), vec![vec![para("Starred")]]),
            Block::Checks(vec![
                (true, vec![para("Done")]),
                (false, vec![para("To do")]),
            ]),
            Block::Quote(vec![para("Quoted")]),
            Block::Code("let a = 1;\n  let b = 2;".into()),
            Block::Rule(Rule {
                thickness: Some(40),
                color: Some(Color([0, 0, 0xff])),
            }),
            Block::Rule(Rule::default()),
            Block::PageBreak,
            Block::Heading(2, Para::default(), vec![text("Second")]),
            Block::Image(Image {
                hash: picture.hash.clone(),
                alt: "A dot".into(),
                width: Some(1),
            }),
            model::table(vec![
                vec![Cell {
                    header: true,
                    colspan: 2,
                    colwidth: Some(vec![200, 100]),
                    content: vec![para("Head")],
                    ..Cell::default()
                }],
                vec![
                    Cell {
                        background: Some(Color([0xd9, 0xea, 0xd3])),
                        rowspan: 2,
                        colwidth: Some(vec![200]),
                        borders: [Some(20), Some(0), Some(10), Some(0)],
                        border_color: Some(Color([0xff, 0, 0])),
                        padding: Some([100, 120, 100, 120]),
                        content: vec![para("Tall")],
                        ..Cell::default()
                    },
                    Cell {
                        colwidth: Some(vec![100]),
                        content: vec![para("B")],
                        ..Cell::default()
                    },
                ],
                vec![Cell {
                    colwidth: Some(vec![100]),
                    content: vec![para("C")],
                    ..Cell::default()
                }],
            ]),
            Block::Heading(6, Para::default(), vec![text("Last")]),
        ]);
        let page = Inline::Text(
            "#".into(),
            Marks {
                field: Some(Field::PageNumber),
                ..Marks::default()
            },
        );
        let count = Inline::Text(
            "#".into(),
            Marks {
                field: Some(Field::PageCount),
                bold: true,
                ..Marks::default()
            },
        );
        let parts = Parts {
            header: vec![para("Header")],
            footer: vec![Block::Paragraph(
                Para {
                    align: Align::Right,
                    ..Para::default()
                },
                vec![text("Page "), page, text(" of "), count],
            )],
            first_header: Vec::new(),
            first_footer: vec![para("First footer")],
            different_first: true,
        }
        .normalized();
        let setup = PageSetup {
            size: PageSize::A4,
            landscape: true,
            margins: [36.0, 54.0, 72.0, 54.0],
        };
        let (read, report) = read_back(&blocks, &parts, &setup, &pictures);
        assert_eq!(report.lost, BTreeSet::new());
        assert_eq!(report.paper, None);
        for (index, block) in blocks.iter().enumerate() {
            assert_eq!(read.blocks.get(index), Some(block), "block {index}");
        }
        assert_eq!(read.blocks.len(), blocks.len());
        assert_eq!(read.parts, parts);
        assert_eq!(read.setup, Some(setup));
        assert_eq!(read.pictures.len(), 1);
        assert!(read.pictures.contains_key(&picture.hash));
        assert_eq!(report.fonts, Vec::new());
    }

    /// An OpenDocument file with `content` as its body and `styles` as its
    /// common styles, laid out as LibreOffice writes one.
    fn odt_file(automatic: &str, body: &str, styles: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write as _;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        let content = format!(
            "<?xml version=\"1.0\"?><office:document-content {NS} office:version=\"1.3\">\
             <office:font-face-decls><style:font-face style:name=\"Liberation Serif\" svg:font-family=\"&apos;Liberation Serif&apos;\"/>\
             <style:font-face style:name=\"Calibri1\" svg:font-family=\"Calibri\"/></office:font-face-decls>\
             <office:automatic-styles>{automatic}</office:automatic-styles>\
             <office:body><office:text>{body}</office:text></office:body></office:document-content>"
        );
        let styles = format!(
            "<?xml version=\"1.0\"?><office:document-styles {NS} office:version=\"1.3\">{styles}</office:document-styles>"
        );
        let manifest = "<?xml version=\"1.0\"?><manifest:manifest xmlns:manifest=\"urn:oasis:names:tc:opendocument:xmlns:manifest:1.0\">\
             <manifest:file-entry manifest:full-path=\"Object 1/\" manifest:media-type=\"application/vnd.oasis.opendocument.formula\"/></manifest:manifest>";
        let mut files: Vec<(&str, Vec<u8>)> = vec![
            (
                "mimetype",
                b"application/vnd.oasis.opendocument.text".to_vec(),
            ),
            ("content.xml", content.into_bytes()),
            ("styles.xml", styles.into_bytes()),
            ("META-INF/manifest.xml", manifest.as_bytes().to_vec()),
        ];
        files.extend(extra.iter().map(|(path, bytes)| (*path, bytes.to_vec())));
        for (path, bytes) in files {
            zip.start_file(path, options).unwrap();
            zip.write_all(&bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    const NS: &str = "xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" \
        xmlns:style=\"urn:oasis:names:tc:opendocument:xmlns:style:1.0\" \
        xmlns:text=\"urn:oasis:names:tc:opendocument:xmlns:text:1.0\" \
        xmlns:table=\"urn:oasis:names:tc:opendocument:xmlns:table:1.0\" \
        xmlns:fo=\"urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0\" \
        xmlns:svg=\"urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0\" \
        xmlns:draw=\"urn:oasis:names:tc:opendocument:xmlns:drawing:1.0\" \
        xmlns:xlink=\"http://www.w3.org/1999/xlink\" \
        xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
        xmlns:xml=\"http://www.w3.org/XML/1998/namespace\"";

    fn plain(blocks: &Block) -> String {
        match blocks {
            Block::Paragraph(_, inlines) | Block::Heading(_, _, inlines) => flat::plain(inlines),
            _ => String::new(),
        }
    }

    #[test]
    fn reads_opendocument_files_as_libreoffice_writes_them() {
        let styles = "<office:font-face-decls><style:font-face style:name=\"Liberation Sans\" svg:font-family=\"&apos;Liberation Sans&apos;\"/></office:font-face-decls>\
            <office:styles>\
            <style:default-style style:family=\"paragraph\"><style:paragraph-properties style:tab-stop-distance=\"1.251cm\"/>\
            <style:text-properties style:font-name=\"Liberation Serif\" fo:font-size=\"12pt\"/></style:default-style>\
            <style:style style:name=\"Standard\" style:family=\"paragraph\" style:class=\"text\"/>\
            <style:style style:name=\"Text_20_body\" style:display-name=\"Text body\" style:family=\"paragraph\" style:parent-style-name=\"Standard\">\
            <style:paragraph-properties fo:margin-top=\"0cm\" fo:margin-bottom=\"0.247cm\" fo:line-height=\"115%\"/></style:style>\
            <style:style style:name=\"Heading\" style:family=\"paragraph\" style:parent-style-name=\"Standard\">\
            <style:text-properties style:font-name=\"Liberation Sans\" fo:font-size=\"14pt\"/></style:style>\
            <style:style style:name=\"Heading_20_1\" style:display-name=\"Heading 1\" style:family=\"paragraph\" style:parent-style-name=\"Heading\" style:default-outline-level=\"1\">\
            <style:text-properties fo:font-size=\"130%\" fo:font-weight=\"bold\"/></style:style>\
            <style:style style:name=\"Quotations\" style:family=\"paragraph\" style:parent-style-name=\"Standard\"/>\
            <style:style style:name=\"Strong_20_Emphasis\" style:display-name=\"Strong Emphasis\" style:family=\"text\"><style:text-properties fo:font-weight=\"bold\"/></style:style>\
            <text:outline-style style:name=\"Outline\"><text:outline-level-style text:level=\"1\" style:num-format=\"1\" style:num-suffix=\".\"/>\
            <text:outline-level-style text:level=\"2\" style:num-format=\"1\" text:display-levels=\"2\"/></text:outline-style>\
            <text:list-style style:name=\"Numbering_20_123\" style:display-name=\"Numbering 123\">\
            <text:list-level-style-number text:level=\"1\" style:num-suffix=\")\" style:num-format=\"1\"/>\
            <text:list-level-style-number text:level=\"2\" style:num-suffix=\".\" style:num-format=\"a\"/></text:list-style>\
            </office:styles>\
            <office:automatic-styles><style:page-layout style:name=\"pm1\">\
            <style:page-layout-properties fo:page-width=\"21.001cm\" fo:page-height=\"29.7cm\" style:print-orientation=\"portrait\" \
            fo:margin-top=\"2cm\" fo:margin-bottom=\"2cm\" fo:margin-left=\"2cm\" fo:margin-right=\"2cm\"/>\
            <style:header-style><style:header-footer-properties fo:min-height=\"0.5cm\" fo:margin-bottom=\"0.25cm\"/></style:header-style>\
            <style:footer-style/></style:page-layout>\
            <style:style style:name=\"MP1\" style:family=\"paragraph\" style:parent-style-name=\"Standard\"><style:paragraph-properties fo:text-align=\"center\"/></style:style>\
            </office:automatic-styles>\
            <office:master-styles><style:master-page style:name=\"Standard\" style:page-layout-name=\"pm1\">\
            <style:header><text:p text:style-name=\"MP1\">Page <text:page-number text:select-page=\"current\">3</text:page-number></text:p></style:header>\
            <style:header-left style:display=\"false\"/>\
            </style:master-page></office:master-styles>";
        let automatic = "<style:style style:name=\"P1\" style:family=\"paragraph\" style:parent-style-name=\"Text_20_body\">\
            <style:paragraph-properties fo:text-align=\"justify\" fo:break-before=\"page\"/></style:style>\
            <style:style style:name=\"T1\" style:family=\"text\"><style:text-properties fo:font-style=\"italic\" style:font-name=\"Calibri1\" fo:color=\"#c9211e\"/></style:style>\
            <style:style style:name=\"T2\" style:family=\"text\"><style:text-properties style:text-position=\"super 58%\"/></style:style>\
            <style:style style:name=\"T3\" style:family=\"text\"><style:text-properties fo:text-transform=\"uppercase\" fo:background-color=\"#ffff00\"/></style:style>\
            <style:style style:name=\"Table1.A1\" style:family=\"table-cell\"><style:table-cell-properties fo:padding=\"0.097cm\" fo:border=\"0.5pt solid #000000\"/></style:style>\
            <style:style style:name=\"Table1.A\" style:family=\"table-column\"><style:table-column-properties style:column-width=\"3cm\"/></style:style>\
            <style:style style:name=\"Sect1\" style:family=\"section\"><style:section-properties><style:columns fo:column-count=\"2\"/></style:section-properties></style:style>";
        let body = "<text:sequence-decls/>\
            <text:h text:style-name=\"Heading_20_1\" text:outline-level=\"1\">Intro</text:h>\
            <text:p text:style-name=\"Text_20_body\">Plain   and\n  <text:span text:style-name=\"Strong_20_Emphasis\">strong</text:span> \
            <text:span text:style-name=\"T1\">red</text:span><text:span text:style-name=\"T2\">2</text:span><text:s text:c=\"2\"/>end\
            <text:note text:id=\"ftn1\" text:note-class=\"footnote\"><text:note-citation>1</text:note-citation><text:note-body>\
            <text:p text:style-name=\"Footnote\">The note</text:p></text:note-body></text:note><text:tab/><text:span text:style-name=\"T3\">loud</text:span>\
            <text:a xlink:type=\"simple\" xlink:href=\"https://example.com/\">site</text:a><text:a xlink:href=\"javascript:x\">bad</text:a>\
            <office:annotation><dc:creator>A</dc:creator><text:p>A comment</text:p></office:annotation></text:p>\
            <text:h text:style-name=\"Heading_20_1\" text:outline-level=\"2\">Sub</text:h>\
            <text:list xml:id=\"list1\" text:style-name=\"Numbering_20_123\"><text:list-item><text:p>First</text:p>\
            <text:list><text:list-item><text:p>Inner</text:p></text:list-item></text:list></text:list-item>\
            <text:list-item><text:p>Second</text:p></text:list-item></text:list>\
            <text:p text:style-name=\"Standard\">Between</text:p>\
            <text:list text:continue-list=\"list1\" text:style-name=\"Numbering_20_123\"><text:list-item><text:p>Third</text:p></text:list-item></text:list>\
            <text:list text:style-name=\"Numbering_20_123\"><text:list-item><text:p>Again</text:p></text:list-item></text:list>\
            <text:p text:style-name=\"P1\">New page<draw:frame draw:name=\"Image1\" text:anchor-type=\"as-char\" svg:width=\"1.5in\" svg:height=\"1in\">\
            <draw:image xlink:href=\"Pictures/dot.png\"/><svg:desc>A dot</svg:desc></draw:frame></text:p>\
            <text:p><draw:frame draw:name=\"Object1\"><draw:object xlink:href=\"./Object 1\"/></draw:frame></text:p>\
            <text:section text:name=\"Section1\" text:style-name=\"Sect1\"><text:p text:style-name=\"Quotations\">Quoted</text:p></text:section>\
            <table:table table:name=\"Table1\"><table:table-column table:style-name=\"Table1.A\" table:number-columns-repeated=\"2\"/>\
            <table:table-header-rows><table:table-row><table:table-cell table:style-name=\"Table1.A1\" table:number-columns-spanned=\"2\"><text:p>Head</text:p></table:table-cell>\
            <table:covered-table-cell/></table:table-row></table:table-header-rows>\
            <table:table-row><table:table-cell table:style-name=\"Table1.A1\" table:number-rows-spanned=\"2\"><text:p>Tall</text:p></table:table-cell>\
            <table:table-cell table:style-name=\"Table1.A1\"><text:p>B</text:p></table:table-cell></table:table-row>\
            <table:table-row><table:covered-table-cell/><table:table-cell><text:p>C</text:p></table:table-cell></table:table-row></table:table>\
            <text:p>Date: <text:date>May 1</text:date><text:bookmark text:name=\"x\"/></text:p>\
            <text:tracked-changes><text:changed-region text:id=\"c1\"><text:deletion><text:p>gone</text:p></text:deletion></text:changed-region></text:tracked-changes>";
        let picture = crate::picture::tests::png(4, 3);
        let file = odt_file(
            automatic,
            body,
            styles,
            &[("Pictures/dot.png", picture.as_slice())],
        );
        let mut report = Report::default();
        let converted = read(&file, &mut report).unwrap();
        let blocks = model::normalize(converted.blocks);

        let Block::Heading(1, _, intro) = &blocks[0] else {
            panic!("{:?}", blocks[0]);
        };
        assert_eq!(flat::plain(intro), "1. Intro");
        let Block::Paragraph(para, inlines) = &blocks[1] else {
            panic!("{:?}", blocks[1]);
        };
        // Text body's spacing is Word's and LibreOffice's 0.247 cm.
        assert_eq!(para.after, Some(140));
        assert_eq!(
            flat::plain(inlines),
            "Plain and strong red2  end LOUDsitebad"
        );
        assert!(
            inlines
                .iter()
                .any(|inline| matches!(inline, Inline::Text(t, m) if t == "strong" && m.bold))
        );
        assert!(inlines.iter().any(
            |inline| matches!(inline, Inline::Text(t, m) if t == "red" && m.italic
            && m.color == Some(Color([0xc9, 0x21, 0x1e])) && m.font.as_deref() == Some("Carlito"))
        ));
        assert!(inlines.iter().any(|inline| matches!(inline, Inline::Text(t, m) if t == "2" && m.script == Some(Script::Super))));
        assert!(
            inlines
                .iter()
                .any(|inline| matches!(inline, Inline::Text(t, m) if t == "LOUD"
            && m.highlight == Some(Color([0xff, 0xff, 0]))))
        );
        assert!(
            inlines
                .iter()
                .any(|inline| matches!(inline, Inline::Text(t, m) if t == "site"
            && m.link.as_deref() == Some("https://example.com/")))
        );
        assert!(
            inlines.iter().any(
                |inline| matches!(inline, Inline::Text(t, m) if t == "bad" && m.link.is_none())
            )
        );
        assert!(inlines.iter().any(
            |inline| matches!(inline, Inline::Footnote(note) if flat::plain(note) == "The note")
        ));
        // Headings are numbered as the outline numbers them.
        assert!(
            matches!(&blocks[2], Block::Heading(2, _, inlines) if flat::plain(inlines) == "1.1 Sub")
        );
        // A list that carries on another goes on with its numbers.
        let Block::Numbers(1, Some(ListStyle::DecimalParen), items) = &blocks[3] else {
            panic!("{:?}", blocks[3]);
        };
        assert_eq!(items.len(), 2);
        assert!(matches!(
            &items[0][1],
            Block::Numbers(1, Some(ListStyle::LowerAlpha), _)
        ));
        assert_eq!(plain(&blocks[4]), "Between");
        assert!(matches!(&blocks[5], Block::Numbers(3, _, _)));
        assert!(matches!(&blocks[6], Block::Numbers(1, _, _)));
        assert!(matches!(&blocks[7], Block::PageBreak));
        assert!(matches!(&blocks[8], Block::Paragraph(para, _) if para.align == Align::Justify));
        assert!(
            matches!(&blocks[9], Block::Image(image) if image.alt == "A dot" && image.width == Some(144))
        );
        assert!(matches!(&blocks[11], Block::Quote(_)));
        let Block::Table(rows) = &blocks[12] else {
            panic!("{:?}", blocks[12]);
        };
        assert_eq!(rows.len(), 3);
        assert!(rows[0].cells[0].header && rows[0].cells[0].colspan == 2);
        assert_eq!(rows[1].cells[0].rowspan, 2);
        assert_eq!(rows[1].cells[0].colwidth, Some(vec![113]));
        assert_eq!(rows[1].cells[0].borders, [Some(10); 4]);
        assert_eq!(rows[2].cells.len(), 1);
        assert_eq!(plain(&blocks[13]), "Date: May 1");

        // A comment marked only where it is comes across, about no text.
        assert_eq!(converted.comments.len(), 1);
        assert_eq!(converted.comments[0].comments[0].name, "A");
        assert_eq!(converted.comments[0].comments[0].text, "A comment");
        assert_eq!(
            report.lost,
            [
                Lost::TrackedChanges,
                Lost::Equations,
                Lost::PicturesMoved,
                Lost::Fields,
                Lost::Columns,
                Lost::Tabs,
            ]
            .into_iter()
            .collect()
        );
        assert!(report.fonts.iter().any(|font| font.from == "Calibri"));
        let parts = converted.parts.normalized();
        assert!(matches!(&parts.header[0], Block::Paragraph(para, inlines)
            if para.align == Align::Center && inlines.iter().any(|inline| matches!(inline, Inline::Text(_, m) if m.field == Some(Field::PageNumber)))));
        let setup = converted.setup.unwrap();
        assert_eq!(setup.size, PageSize::A4);
        // The header's room is the margin's: 2 cm, then 0.75 cm for it.
        assert!(
            (setup.margins[0] - (2.75 * 72.0 / 2.54)).abs() < 0.1,
            "{setup:?}"
        );
        assert_eq!(report.paper, None);
    }

    #[test]
    fn stops_at_tables_that_repeat_without_end() {
        let body = "<table:table><table:table-column table:number-columns-repeated=\"1000000\"/>\
            <table:table-row table:number-rows-repeated=\"1000000\"><table:table-cell table:number-columns-repeated=\"1000000\">\
            <text:p>x</text:p></table:table-cell></table:table-row></table:table>";
        let file = odt_file("", body, "", &[]);
        let mut report = Report::default();
        let converted = read(&file, &mut report).unwrap();
        let Block::Table(rows) = &converted.blocks[0] else {
            panic!("{:?}", converted.blocks[0]);
        };
        let cells: usize = rows.iter().map(|row| row.cells.len()).sum();
        assert_eq!(cells, MAX_CELLS);
    }

    #[test]
    fn refuses_files_that_arent_text_documents() {
        let mut report = Report::default();
        assert!(read(b"not a zip", &mut report).is_err());
        let sheet = {
            use std::io::Write as _;
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            zip.start_file("content.xml", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(
                format!("<office:document-content {NS}><office:body><office:spreadsheet/></office:body></office:document-content>")
                    .as_bytes(),
            )
            .unwrap();
            zip.finish().unwrap().into_inner()
        };
        assert!(read(&sheet, &mut report).is_err());
    }
}
