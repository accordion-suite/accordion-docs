//! Reads Word files (`.docx`) into Docs' schema, for **Open with Docs**
//! (decision 30). It runs in the sandbox, since the file is untrusted.
//!
//! It keeps what Docs has: headings, the title and subtitle, text styles
//! and fonts from Word's styles and the text itself, paragraph layout,
//! lists with their bullets and numbers, checklists written as ☐ and ☑,
//! quotes, code, lines, tables with merged cells, borders, colors, and
//! column widths, links, pictures, footnotes, headers and footers with page
//! numbers and counts, a table of contents, page breaks, and the paper and
//! margins. Comments come too, with their replies, as Google Docs and
//! Word write them, each about the text it was about. Anything else is
//! left out or made plainer, and the [`Report`] says what, for the warning
//! before converting.
//!
//! A Word file Docs wrote comes back as it was: Docs' own styles, list
//! numberings, and table style read as Docs' defaults.

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
    PageSetup, PageSize, Para, Parts, Row, Rule, Script, Twips,
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

/// The file's text, pictures, and page, and what didn't come across.
pub(crate) fn read(bytes: &[u8], report: &mut Report) -> Result<Converted, String> {
    let mut package = Package::open(bytes)?;
    let main = package
        .relations("")
        .values()
        .find(|relation| relation.kind == "officeDocument")
        .map(|relation| relation.target.clone())
        .unwrap_or_else(|| "word/document.xml".into());
    let relations = package.relations(&main);
    let document = package
        .part(&main)?
        .ok_or("has no document in it, so it may not be a Word file")?;
    let by_kind = |kind: &str| {
        relations
            .values()
            .find(|relation| relation.kind == kind && !relation.external)
            .map(|relation| relation.target.clone())
    };
    let styles = match by_kind("styles") {
        Some(path) => package.part(&path)?,
        None => None,
    };
    let theme = match by_kind("theme") {
        Some(path) => package.part(&path)?,
        None => None,
    };
    let theme = Theme::read(theme.as_ref());
    let styles = Styles::read(styles.as_ref(), &theme);
    let numbering = match by_kind("numbering") {
        Some(path) => package.part(&path)?,
        None => None,
    };
    let numbering = Numbering::read(numbering.as_ref());
    let mut notes = HashMap::new();
    for (kind, endnote) in [("footnotes", false), ("endnotes", true)] {
        let Some(path) = by_kind(kind) else { continue };
        let Some(root) = package.part(&path)? else {
            continue;
        };
        let note_relations = package.relations(&path);
        for note in root.into_elements() {
            let Some(id) = note.attr(Ns::W, "id").map(str::to_owned) else {
                continue;
            };
            if note
                .attr(Ns::W, "type")
                .is_some_and(|kind| kind != "normal")
            {
                continue;
            }
            notes.insert((endnote, id), (note, path.clone(), note_relations.clone()));
        }
    }
    let mut part_of = |kind: &str| match by_kind(kind) {
        Some(path) => package.part(&path),
        None => Ok(None),
    };
    let comment_parts = [
        part_of("comments")?,
        part_of("commentsExtended")?,
        part_of("commentsIds")?,
        part_of("commentsExtensible")?,
    ];
    let (threads, comment_threads) = comments(comment_parts);
    let even_pages = match by_kind("settings") {
        Some(path) => package
            .part(&path)?
            .is_some_and(|settings| on(settings.child(Ns::W, "evenAndOddHeaders"))),
        None => false,
    };

    let mut reader = Reader {
        package,
        part: main.clone(),
        relations,
        styles,
        numbering,
        theme,
        notes,
        pictures: Pictures::new(),
        media: HashMap::new(),
        counters: HashMap::new(),
        fields: Vec::new(),
        contents: Vec::new(),
        dotted: false,
        fonts: BTreeSet::new(),
        lost: BTreeSet::new(),
        in_note: false,
        comment_threads,
        placed: HashSet::new(),
        between: Vec::new(),
    };
    let body = document
        .child(Ns::W, "body")
        .ok_or("has no body in it, so it may not be a Word file")?;
    let flats = reader.container(&body.children, &Context::default());
    let mut blocks = group(flats);
    // Each table of contents, in order, gets the look its entries had.
    let mut styles = reader.contents.iter();
    for block in &mut blocks {
        if let Block::Contents(style) = block
            && let Some(found) = styles.next()
        {
            *style = *found;
        }
    }

    // The first section's paper and margins, and its headers and footers.
    let sections: Vec<&Element> = body
        .elements()
        .filter_map(|element| {
            if element.is(Ns::W, "p") {
                element.child(Ns::W, "pPr")?.child(Ns::W, "sectPr")
            } else if element.is(Ns::W, "sectPr") {
                Some(element)
            } else {
                None
            }
        })
        .collect();
    let mut setup = None;
    let mut parts = Parts::default();
    if let Some(first) = sections.first() {
        setup = Some(page_setup(first, report));
        let differs = sections.iter().skip(1).any(|section| {
            page_setup(section, &mut Report::default()) != *setup.as_ref().expect("set")
        });
        if differs {
            reader.lost.insert(Lost::Sections);
        }
        if sections.iter().any(|section| {
            section
                .child(Ns::W, "cols")
                .and_then(|cols| cols.attr(Ns::W, "num"))
                .and_then(|num| num.parse::<u32>().ok())
                .is_some_and(|num| num > 1)
        }) {
            reader.lost.insert(Lost::Columns);
        }
        parts.different_first = on(first.child(Ns::W, "titlePg"));
        for reference in first.elements() {
            let header = reference.is(Ns::W, "headerReference");
            if !header && !reference.is(Ns::W, "footerReference") {
                continue;
            }
            let kind = reference.attr(Ns::W, "type").unwrap_or("default");
            let target = match kind {
                "default" if header => &mut parts.header,
                "default" => &mut parts.footer,
                "first" if header => &mut parts.first_header,
                "first" => &mut parts.first_footer,
                _ => {
                    if even_pages {
                        reader.lost.insert(Lost::EvenPages);
                    }
                    continue;
                }
            };
            let Some(id) = reference.attr(Ns::R, "id") else {
                continue;
            };
            let Some(relation) = reader.relations.get(id).cloned() else {
                continue;
            };
            let Some(root) = reader.package.part(&relation.target)? else {
                continue;
            };
            let relations = reader.package.relations(&relation.target);
            let saved = (
                std::mem::replace(&mut reader.part, relation.target.clone()),
                std::mem::replace(&mut reader.relations, relations),
            );
            let flats = reader.container(&root.children, &Context::default());
            *target = group(flats);
            (reader.part, reader.relations) = saved;
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

/// The file's comments as threads, from its comments part, and the parts
/// Word 2013 and later add: which comment answers which and which are done
/// (`commentsExtended`), and each one's time in UTC (`commentsIds` and
/// `commentsExtensible`), since `w:date` is the writer's local time in
/// Word's files. Also the thread each first comment's ID starts, for the
/// marks in the text; replies have their own marks, which Docs skips.
fn comments(parts: [Option<Element>; 4]) -> (Vec<Thread>, HashMap<String, Option<String>>) {
    let [comments, extended, ids, extensible] = parts;
    let Some(comments) = comments else {
        return (Vec::new(), HashMap::new());
    };
    // Each comment's ID, its last paragraph's ID, and what it says.
    let mut read: Vec<(String, Option<String>, FileComment)> = Vec::new();
    for comment in comments.elements().filter(|e| e.is(Ns::W, "comment")) {
        let Some(id) = comment.attr(Ns::W, "id") else {
            continue;
        };
        let paragraphs = comment.descendants(Ns::W, "p");
        let paragraph = paragraphs
            .last()
            .and_then(|p| p.attr(Ns::W14, "paraId"))
            .map(str::to_ascii_uppercase);
        let text: Vec<String> = paragraphs.iter().map(|p| comment_text(p)).collect();
        read.push((
            id.to_owned(),
            paragraph,
            FileComment {
                name: comment.attr(Ns::W, "author").unwrap_or_default().to_owned(),
                text: text.join("\n"),
                at: comment.attr(Ns::W, "date").map(str::to_owned),
            },
        ));
    }
    // Which paragraph each answers, and whether it's done, by paragraph.
    let mut answers: HashMap<String, String> = HashMap::new();
    let mut done: HashSet<String> = HashSet::new();
    for entry in extended.iter().flat_map(|root| root.elements()) {
        let Some(paragraph) = entry.attr(Ns::W15, "paraId") else {
            continue;
        };
        let paragraph = paragraph.to_ascii_uppercase();
        if let Some(parent) = entry.attr(Ns::W15, "paraIdParent") {
            answers.insert(paragraph.clone(), parent.to_ascii_uppercase());
        }
        if entry.attr(Ns::W15, "done") == Some("1") {
            done.insert(paragraph);
        }
    }
    let mut durable: HashMap<String, String> = HashMap::new();
    for entry in ids.iter().flat_map(|root| root.elements()) {
        if let (Some(paragraph), Some(id)) = (
            entry.attr(Ns::W16Cid, "paraId"),
            entry.attr(Ns::W16Cid, "durableId"),
        ) {
            durable.insert(paragraph.to_ascii_uppercase(), id.to_ascii_uppercase());
        }
    }
    let mut utc: HashMap<String, String> = HashMap::new();
    for entry in extensible.iter().flat_map(|root| root.elements()) {
        if let (Some(id), Some(at)) = (
            entry.attr(Ns::W16Cex, "durableId"),
            entry.attr(Ns::W16Cex, "dateUtc"),
        ) {
            utc.insert(id.to_ascii_uppercase(), at.to_owned());
        }
    }
    let by_paragraph: HashMap<&str, usize> = read
        .iter()
        .enumerate()
        .filter_map(|(index, (_, paragraph, _))| Some((paragraph.as_deref()?, index)))
        .collect();
    // Each comment's first comment: itself, or the one its answers lead to.
    let first_of = |index: usize| {
        let mut at = index;
        for _ in 0..read.len() {
            let Some(parent) = read[at]
                .1
                .as_ref()
                .and_then(|paragraph| answers.get(paragraph))
                .and_then(|parent| by_paragraph.get(parent.as_str()))
            else {
                break;
            };
            if *parent == at {
                break;
            }
            at = *parent;
        }
        at
    };
    let firsts: Vec<usize> = (0..read.len()).map(first_of).collect();
    let mut grouped: Vec<(usize, Vec<usize>)> = Vec::new();
    for (index, first) in firsts.iter().enumerate() {
        match grouped.iter_mut().find(|(top, _)| top == first) {
            Some((_, members)) => members.push(index),
            None => grouped.push((*first, vec![index])),
        }
    }
    let mut by_word_id: HashMap<String, Option<String>> = HashMap::new();
    let mut taken: Vec<Option<FileComment>> = Vec::new();
    let mut keys = Vec::new();
    for (id, paragraph, comment) in read {
        keys.push((id, paragraph));
        taken.push(Some(comment));
    }
    let mut threads = Vec::new();
    for (first, mut members) in grouped {
        // The first comment leads, whatever the order in the file.
        members.sort_by_key(|member| (*member != first, *member));
        let resolved = members.iter().any(|member| {
            keys[*member]
                .1
                .as_ref()
                .is_some_and(|paragraph| done.contains(paragraph))
        });
        let comments: Vec<FileComment> = members
            .iter()
            .filter_map(|member| {
                let mut comment = taken[*member].take()?;
                if let Some(at) = keys[*member]
                    .1
                    .as_ref()
                    .and_then(|paragraph| durable.get(paragraph))
                    .and_then(|id| utc.get(id))
                {
                    comment.at = Some(at.clone());
                }
                Some(comment)
            })
            .collect();
        let thread = file_thread(comments, resolved);
        for member in &members {
            let thread_id = (*member == first).then(|| thread.id.clone());
            by_word_id.insert(keys[*member].0.clone(), thread_id);
        }
        threads.push(thread);
    }
    (threads, by_word_id)
}

/// A comment paragraph's text, with tabs and line breaks.
fn comment_text(paragraph: &Element) -> String {
    let mut text = String::new();
    let mut waiting: Vec<&Element> = paragraph.elements().collect();
    waiting.reverse();
    while let Some(element) = waiting.pop() {
        match (element.ns, element.name.as_str()) {
            (Some(Ns::W), "t") => text.push_str(&element.text()),
            (Some(Ns::W), "tab") => text.push(' '),
            (Some(Ns::W), "br" | "cr") => text.push('\n'),
            (Some(Ns::W), "del" | "delText" | "instrText") => {}
            _ => {
                let start = waiting.len();
                waiting.extend(element.elements());
                waiting[start..].reverse();
            }
        }
    }
    text
}

/// The paper and margins of a section's `w:sectPr`.
fn page_setup(section: &Element, report: &mut Report) -> PageSetup {
    let twips = |element: Option<&Element>, name: &str| {
        element
            .and_then(|element| element.attr(Ns::W, name))
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite())
    };
    let size = section.child(Ns::W, "pgSz");
    let width = twips(size, "w").map(|w| w / 20.0);
    let height = twips(size, "h").map(|h| h / 20.0);
    let landscape = size.and_then(|size| size.attr(Ns::W, "orient")) == Some("landscape")
        || matches!((width, height), (Some(w), Some(h)) if w > h);
    let mut paper = PageSize::Letter;
    if let (Some(w), Some(h)) = (width, height) {
        paper = flat::paper(w, h, report).0;
    }
    let margins = section.child(Ns::W, "pgMar");
    let margin =
        |name: &str| twips(margins, name).map_or(model::DEFAULT_MARGIN, |value| value.abs() / 20.0);
    PageSetup {
        size: paper,
        landscape,
        margins: [
            margin("top"),
            margin("right"),
            margin("bottom"),
            margin("left"),
        ],
    }
    .normalized()
}

/// A Word on-off property, such as `<w:b/>` or `<w:b w:val="0"/>`: on
/// unless its value says off.
fn on(element: Option<&Element>) -> bool {
    element.is_some_and(|element| {
        !matches!(
            element.val().map(str::to_ascii_lowercase).as_deref(),
            Some("0" | "false" | "off" | "none")
        )
    })
}

fn number<T: std::str::FromStr>(element: Option<&Element>, name: &str) -> Option<T> {
    element?.attr(Ns::W, name)?.trim().parse().ok()
}

/// A color Word gives as six hex digits, unless it's "auto".
fn color(value: Option<&str>) -> Option<Color> {
    let value = value?.trim();
    if value.eq_ignore_ascii_case("auto") {
        return None;
    }
    Color::from_hex(&format!("#{value}"))
}

/// Word's 16 highlight colors, by name.
fn highlight(name: &str) -> Option<Color> {
    let hex = match name {
        "yellow" => "#ffff00",
        "green" => "#00ff00",
        "cyan" => "#00ffff",
        "magenta" => "#ff00ff",
        "blue" => "#0000ff",
        "red" => "#ff0000",
        "darkBlue" => "#000080",
        "darkCyan" => "#008080",
        "darkGreen" => "#008000",
        "darkMagenta" => "#800080",
        "darkRed" => "#800000",
        "darkYellow" => "#808000",
        "darkGray" => "#808080",
        "lightGray" => "#c0c0c0",
        "black" => "#000000",
        "white" => "#ffffff",
        _ => return None,
    };
    Color::from_hex(hex)
}

/// The file's parts, read from its zip.
struct Package<'a> {
    zip: ZipArchive<Cursor<&'a [u8]>>,
}

/// Where a part's relationship leads.
#[derive(Clone, Debug)]
struct Relation {
    /// The last part of its type, such as "styles" or "image".
    kind: String,
    /// The part it leads to, as a path in the zip, or an address outside.
    target: String,
    external: bool,
}

type Relations = HashMap<String, Relation>;

impl<'a> Package<'a> {
    fn open(bytes: &'a [u8]) -> Result<Self, String> {
        let zip = ZipArchive::new(Cursor::new(bytes))
            .map_err(|_| "isn't a Word file, or it's damaged".to_owned())?;
        Ok(Self { zip })
    }

    fn bytes(&mut self, path: &str, limit: u64) -> Option<Vec<u8>> {
        let entry = self.zip.by_name(path).ok()?;
        let mut out = Vec::new();
        entry.take(limit + 1).read_to_end(&mut out).ok()?;
        (out.len() as u64 <= limit).then_some(out)
    }

    /// The part at `path` as XML, or `None` if the file hasn't one.
    fn part(&mut self, path: &str) -> Result<Option<Element>, String> {
        let entry = match self.zip.by_name(path) {
            Ok(entry) => entry,
            Err(_) => return Ok(None),
        };
        xml::parse(BufReader::new(entry.take(MAX_PART)))
            .map(Some)
            .map_err(|problem| format!("{path} {problem}"))
    }

    /// The relationships of the part at `path`, or of the package for "".
    fn relations(&mut self, path: &str) -> Relations {
        let (folder, name) = match path.rsplit_once('/') {
            Some((folder, name)) => (folder, name),
            None => ("", path),
        };
        let rels = if path.is_empty() {
            "_rels/.rels".to_owned()
        } else if folder.is_empty() {
            format!("_rels/{name}.rels")
        } else {
            format!("{folder}/_rels/{name}.rels")
        };
        let Ok(Some(root)) = self.part(&rels) else {
            return Relations::new();
        };
        let mut out = Relations::new();
        for relation in root.elements() {
            let (Some(id), Some(kind), Some(target)) = (
                relation.attr(Ns::None, "Id"),
                relation.attr(Ns::None, "Type"),
                relation.attr(Ns::None, "Target"),
            ) else {
                continue;
            };
            let external = relation.attr(Ns::None, "TargetMode") == Some("External");
            let target = if external {
                target.to_owned()
            } else {
                join(folder, target)
            };
            out.insert(
                id.to_owned(),
                Relation {
                    kind: kind.rsplit('/').next().unwrap_or_default().to_owned(),
                    target,
                    external,
                },
            );
        }
        out
    }
}

/// A relationship's target as a path in the zip, from the folder of the
/// part it belongs to.
fn join(folder: &str, target: &str) -> String {
    let target = target.split(['#', '?']).next().unwrap_or_default();
    let mut parts: Vec<&str> = if let Some(absolute) = target.strip_prefix('/') {
        return absolute.to_owned();
    } else {
        folder.split('/').filter(|part| !part.is_empty()).collect()
    };
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

impl Element {
    fn into_elements(self) -> impl Iterator<Item = Element> {
        self.children.into_iter().filter_map(|node| match node {
            Node::Element(element) => Some(element),
            Node::Text(_) => None,
        })
    }
}

/// The theme's fonts, which styles name as "major" (headings) and "minor"
/// (body text).
#[derive(Default)]
struct Theme {
    major: Option<String>,
    minor: Option<String>,
}

impl Theme {
    fn read(theme: Option<&Element>) -> Self {
        let Some(theme) = theme else {
            return Self::default();
        };
        let font = |name: &str| {
            theme
                .descendants(Ns::A, name)
                .first()
                .and_then(|font| font.child(Ns::A, "latin"))
                .and_then(|latin| latin.attr(Ns::None, "typeface"))
                .filter(|face| !face.is_empty())
                .map(str::to_owned)
        };
        Self {
            major: font("majorFont"),
            minor: font("minorFont"),
        }
    }
}

/// A run's look, as a style or the text itself sets it. Each part is set
/// only where something said so, so later ones override earlier ones.
#[derive(Clone, Debug, Default, PartialEq)]
struct RunProps {
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    caps: Option<bool>,
    hidden: Option<bool>,
    script: Option<Option<Script>>,
    color: Option<Option<Color>>,
    highlight: Option<Option<Color>>,
    shading: Option<Option<Color>>,
    size: Option<HalfPoints>,
    font: Option<String>,
}

impl RunProps {
    fn read(rpr: Option<&Element>, theme: &Theme) -> Self {
        let mut props = Self::default();
        let Some(rpr) = rpr else {
            return props;
        };
        for element in rpr.elements() {
            if element.ns != Some(Ns::W) {
                continue;
            }
            match element.name.as_str() {
                "b" => props.bold = Some(on(Some(element))),
                "i" => props.italic = Some(on(Some(element))),
                "strike" | "dstrike" => props.strike = Some(on(Some(element))),
                "caps" => props.caps = Some(on(Some(element))),
                "vanish" => props.hidden = Some(on(Some(element))),
                "u" => {
                    props.underline = Some(!matches!(element.val(), Some("none" | "0" | "false")))
                }
                "vertAlign" => {
                    props.script = Some(match element.val() {
                        Some("superscript") => Some(Script::Super),
                        Some("subscript") => Some(Script::Sub),
                        _ => None,
                    })
                }
                "color" => props.color = Some(color(element.val())),
                "highlight" => props.highlight = Some(element.val().and_then(highlight)),
                "shd" => props.shading = Some(color(element.attr(Ns::W, "fill"))),
                "sz" => {
                    props.size = element
                        .val()
                        .and_then(|size| size.trim().parse::<f64>().ok())
                        .filter(|size| size.is_finite() && *size >= 1.0)
                        .map(|size| size.round().min(f64::from(u16::MAX)) as HalfPoints)
                }
                "rFonts" => {
                    let themed = ["asciiTheme", "hAnsiTheme"]
                        .iter()
                        .find_map(|name| element.attr(Ns::W, name))
                        .and_then(|name| {
                            if name.starts_with("major") {
                                theme.major.clone()
                            } else {
                                theme.minor.clone()
                            }
                        });
                    let named = ["ascii", "hAnsi"]
                        .iter()
                        .find_map(|name| element.attr(Ns::W, name))
                        .filter(|name| !name.trim().is_empty())
                        .map(str::to_owned);
                    if let Some(font) = themed.or(named) {
                        props.font = Some(font);
                    }
                }
                _ => {}
            }
        }
        props
    }

    /// `self` with `other`'s settings on top.
    fn over(&mut self, other: &RunProps) {
        macro_rules! take {
            ($($field:ident),*) => {$(
                if other.$field.is_some() {
                    self.$field = other.$field.clone();
                }
            )*};
        }
        take!(
            bold, italic, underline, strike, caps, hidden, script, color, highlight, shading, size,
            font
        );
    }
}

/// How lines in a paragraph are spaced.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Spacing {
    /// In 240ths of a line.
    Lines(i32),
    /// Twips from line to line, at least or exactly.
    Twips(i32),
}

/// A paragraph's layout, as a style, a list level, or the paragraph sets it.
#[derive(Clone, Debug, Default, PartialEq)]
struct ParaProps {
    align: Option<Align>,
    left: Option<Twips>,
    right: Option<Twips>,
    /// The first line's indent, below zero when it hangs.
    first: Option<Twips>,
    line: Option<Spacing>,
    before: Option<Twips>,
    after: Option<Twips>,
    outline: Option<u8>,
    /// The list it's in, and its level.
    numbering: Option<(String, u8)>,
    break_before: Option<bool>,
}

impl ParaProps {
    fn read(ppr: Option<&Element>) -> Self {
        let mut props = Self::default();
        let Some(ppr) = ppr else {
            return props;
        };
        for element in ppr.elements() {
            if element.ns != Some(Ns::W) {
                continue;
            }
            let twips = |name: &str| number::<f64>(Some(element), name).map(|v| v.round() as Twips);
            match element.name.as_str() {
                "jc" => props.align = element.val().and_then(Align::from_name),
                "ind" => {
                    if let Some(left) = twips("left").or_else(|| twips("start")) {
                        props.left = Some(left);
                    }
                    if let Some(right) = twips("right").or_else(|| twips("end")) {
                        props.right = Some(right);
                    }
                    if let Some(hanging) = twips("hanging") {
                        props.first = Some(-hanging);
                    } else if let Some(first) = twips("firstLine") {
                        props.first = Some(first);
                    }
                }
                "spacing" => {
                    let auto =
                        |name: &str| matches!(element.attr(Ns::W, name), Some("1" | "true" | "on"));
                    if let Some(before) = twips("before") {
                        props.before = Some(before);
                    }
                    if auto("beforeAutospacing") {
                        props.before = Some(280);
                    }
                    if let Some(after) = twips("after") {
                        props.after = Some(after);
                    }
                    if auto("afterAutospacing") {
                        props.after = Some(280);
                    }
                    if let Some(line) = twips("line") {
                        props.line = Some(match element.attr(Ns::W, "lineRule") {
                            Some("exact" | "atLeast") => Spacing::Twips(line),
                            _ => Spacing::Lines(line),
                        });
                    }
                }
                "outlineLvl" => props.outline = element.val().and_then(|v| v.parse().ok()),
                "numPr" => {
                    let id = element
                        .child(Ns::W, "numId")
                        .and_then(Element::val)
                        .map(str::to_owned);
                    let level = element
                        .child(Ns::W, "ilvl")
                        .and_then(Element::val)
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    if let Some(id) = id {
                        props.numbering = Some((id, level));
                    }
                }
                "pageBreakBefore" => props.break_before = Some(on(Some(element))),
                _ => {}
            }
        }
        props
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
            outline,
            numbering,
            break_before
        );
    }
}

/// A side of a table's or cell's border.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Border {
    /// Twips, zero for none.
    thickness: Twips,
    color: Option<Color>,
}

fn border(element: &Element) -> Border {
    let none = matches!(element.val(), Some("nil" | "none") | None);
    let size: i32 = number(Some(element), "sz").unwrap_or(4);
    Border {
        // Eighths of a point.
        thickness: if none { 0 } else { (size * 5 / 2).max(1) },
        color: color(element.attr(Ns::W, "color")),
    }
}

/// A table's borders: each side of the table, and between cells.
#[derive(Clone, Debug, Default)]
struct Borders {
    top: Option<Border>,
    left: Option<Border>,
    bottom: Option<Border>,
    right: Option<Border>,
    inside_h: Option<Border>,
    inside_v: Option<Border>,
}

impl Borders {
    fn read(element: Option<&Element>) -> Self {
        let mut out = Self::default();
        let Some(element) = element else {
            return out;
        };
        for side in element.elements() {
            let found = Some(border(side));
            match side.name.as_str() {
                "top" => out.top = found,
                "left" | "start" => out.left = found,
                "bottom" => out.bottom = found,
                "right" | "end" => out.right = found,
                "insideH" => out.inside_h = found,
                "insideV" => out.inside_v = found,
                _ => {}
            }
        }
        out
    }

    fn over(&mut self, other: &Borders) {
        for (mine, theirs) in [
            (&mut self.top, other.top),
            (&mut self.left, other.left),
            (&mut self.bottom, other.bottom),
            (&mut self.right, other.right),
            (&mut self.inside_h, other.inside_h),
            (&mut self.inside_v, other.inside_v),
        ] {
            if theirs.is_some() {
                *mine = theirs;
            }
        }
    }
}

/// A style from the styles part.
#[derive(Default)]
struct Style {
    /// Its name in lowercase, such as "heading 1".
    name: String,
    based_on: Option<String>,
    para: ParaProps,
    run: RunProps,
    borders: Borders,
}

struct Styles {
    by_id: HashMap<String, Style>,
    /// The paragraph style paragraphs without one have, usually Normal.
    default_para: Option<String>,
    run: RunProps,
    para: ParaProps,
}

impl Styles {
    fn read(styles: Option<&Element>, theme: &Theme) -> Self {
        let mut out = Self {
            by_id: HashMap::new(),
            default_para: None,
            run: RunProps::default(),
            para: ParaProps::default(),
        };
        // Word's own defaults, when the file says nothing: Times New Roman
        // 10 point, single spaced.
        out.run.size = Some(20);
        out.run.font = Some("Times New Roman".into());
        let Some(styles) = styles else {
            return out;
        };
        if let Some(defaults) = styles.child(Ns::W, "docDefaults") {
            let rpr = defaults
                .child(Ns::W, "rPrDefault")
                .and_then(|default| default.child(Ns::W, "rPr"));
            out.run.over(&RunProps::read(rpr, theme));
            let ppr = defaults
                .child(Ns::W, "pPrDefault")
                .and_then(|default| default.child(Ns::W, "pPr"));
            out.para.over(&ParaProps::read(ppr));
        }
        for style in styles.elements().filter(|e| e.is(Ns::W, "style")) {
            let Some(id) = style.attr(Ns::W, "styleId") else {
                continue;
            };
            let kind = style.attr(Ns::W, "type").unwrap_or("paragraph");
            if kind == "paragraph" && style.attr(Ns::W, "default") == Some("1") {
                out.default_para = Some(id.to_owned());
            }
            let tblpr = style.child(Ns::W, "tblPr");
            out.by_id.insert(
                id.to_owned(),
                Style {
                    name: style
                        .child(Ns::W, "name")
                        .and_then(Element::val)
                        .unwrap_or(id)
                        .to_ascii_lowercase(),
                    based_on: style
                        .child(Ns::W, "basedOn")
                        .and_then(Element::val)
                        .map(str::to_owned),
                    para: ParaProps::read(style.child(Ns::W, "pPr")),
                    run: RunProps::read(style.child(Ns::W, "rPr"), theme),
                    borders: Borders::read(
                        tblpr.and_then(|tblpr| tblpr.child(Ns::W, "tblBorders")),
                    ),
                },
            );
        }
        out
    }

    /// A style and the ones it's based on, the most basic first.
    fn chain(&self, id: Option<&str>) -> Vec<&Style> {
        let mut chain = Vec::new();
        let mut next = id;
        while let Some(id) = next {
            let Some(style) = self.by_id.get(id) else {
                break;
            };
            if chain.len() >= MAX_CHAIN {
                break;
            }
            chain.push(style);
            next = style.based_on.as_deref();
        }
        chain.reverse();
        chain
    }
}

/// A list level from the numbering part.
#[derive(Clone, Debug, Default)]
struct Level {
    /// Word's number format, such as "decimal" or "bullet".
    format: String,
    /// The text, with `%1` for the first level's number and so on.
    text: String,
    start: u32,
    para: ParaProps,
}

/// A list's own start, and its own level, for some of its levels.
type Overrides = HashMap<u8, (Option<u32>, Option<Level>)>;

#[derive(Default)]
struct Numbering {
    /// Each list's definition, and its own levels' starts and levels.
    nums: HashMap<String, (String, Overrides)>,
    abstracts: HashMap<String, (HashMap<u8, Level>, Option<String>)>,
}

impl Numbering {
    fn read(numbering: Option<&Element>) -> Self {
        let mut out = Self::default();
        let Some(numbering) = numbering else {
            return out;
        };
        let level = |lvl: &Element| Level {
            format: lvl
                .child(Ns::W, "numFmt")
                .and_then(Element::val)
                .unwrap_or("decimal")
                .to_owned(),
            text: lvl
                .child(Ns::W, "lvlText")
                .and_then(Element::val)
                .unwrap_or_default()
                .to_owned(),
            start: lvl
                .child(Ns::W, "start")
                .and_then(Element::val)
                .and_then(|v| v.parse().ok())
                .unwrap_or(1),
            para: ParaProps::read(lvl.child(Ns::W, "pPr")),
        };
        let index = |lvl: &Element| {
            lvl.attr(Ns::W, "ilvl")
                .and_then(|v| v.parse::<u8>().ok())
                .filter(|v| *v <= 8)
        };
        for element in numbering.elements() {
            if element.is(Ns::W, "abstractNum") {
                let Some(id) = element.attr(Ns::W, "abstractNumId") else {
                    continue;
                };
                let levels = element
                    .elements()
                    .filter(|e| e.is(Ns::W, "lvl"))
                    .filter_map(|lvl| Some((index(lvl)?, level(lvl))))
                    .collect();
                let link = element
                    .child(Ns::W, "numStyleLink")
                    .and_then(Element::val)
                    .map(str::to_owned);
                out.abstracts.insert(id.to_owned(), (levels, link));
            } else if element.is(Ns::W, "num") {
                let (Some(id), Some(abstract_id)) = (
                    element.attr(Ns::W, "numId"),
                    element.child(Ns::W, "abstractNumId").and_then(Element::val),
                ) else {
                    continue;
                };
                let overrides = element
                    .elements()
                    .filter(|e| e.is(Ns::W, "lvlOverride"))
                    .filter_map(|over| {
                        let start = over
                            .child(Ns::W, "startOverride")
                            .and_then(Element::val)
                            .and_then(|v| v.parse().ok());
                        let lvl = over.child(Ns::W, "lvl").map(level);
                        Some((index(over)?, (start, lvl)))
                    })
                    .collect();
                out.nums
                    .insert(id.to_owned(), (abstract_id.to_owned(), overrides));
            }
        }
        out
    }
}

/// Where blocks are being read.
#[derive(Clone, Default)]
struct Context {
    /// A table style's paragraph layout and text, under the paragraph's.
    table: Option<(ParaProps, RunProps)>,
}

/// A field from `w:fldChar`s, while it's being read.
struct FieldState {
    instruction: String,
    /// Past its instruction, into the result Word last showed.
    result: bool,
    action: Action,
}

#[derive(Clone, Debug, PartialEq)]
enum Action {
    /// Its result stays as text.
    Keep,
    /// A page number or count: Docs' own field in place of its result.
    Number(Field),
    /// A link around its result.
    Link(Option<String>),
    /// A table of contents: Docs' own in place of its entries.
    Contents,
}

/// What a field's instruction does in Docs.
fn action(instruction: &str, lost: &mut BTreeSet<Lost>) -> Action {
    let mut words = instruction.split_whitespace();
    let name = words.next().unwrap_or_default().to_ascii_uppercase();
    match name.as_str() {
        "PAGE" => Action::Number(Field::PageNumber),
        "NUMPAGES" | "SECTIONPAGES" => Action::Number(Field::PageCount),
        "HYPERLINK" => {
            let rest: Vec<&str> = words.collect();
            let address = rest
                .iter()
                .position(|word| !word.starts_with('\\'))
                .filter(|at| *at == 0 || !rest[at - 1].starts_with('\\'))
                .map(|at| rest[at].trim_matches('"'));
            Action::Link(address.and_then(model::safe_link))
        }
        "TOC" => Action::Contents,
        "PAGEREF" | "REF" | "NOTEREF" | "SEQ" | "STYLEREF" | "" => Action::Keep,
        _ => {
            lost.insert(Lost::Fields);
            Action::Keep
        }
    }
}

/// Inline content as it's read, broken where a block has to go between,
/// such as a picture or a page break.
#[derive(Default)]
struct Pieces {
    inlines: Vec<Inline>,
    out: Vec<Piece>,
    /// Blocks read from text boxes, which go after the paragraph.
    after: Vec<Flat>,
    /// Whether any text or picture came from the paragraph, as opposed to
    /// only fields being skipped.
    shown: bool,
}

impl Pieces {
    fn block(&mut self, block: Block) {
        let inlines = std::mem::take(&mut self.inlines);
        self.out.push(Piece::Text(inlines));
        self.out.push(Piece::Block(block));
        self.shown = true;
    }
}

/// How a run's text is marked, apart from its own properties.
#[derive(Clone)]
struct RunContext {
    /// The paragraph's text properties, from the defaults and its style.
    base: RunProps,
    role: Role,
    link: Option<String>,
}

struct Reader<'a> {
    package: Package<'a>,
    /// The part being read, and its relationships.
    part: String,
    relations: Relations,
    styles: Styles,
    numbering: Numbering,
    theme: Theme,
    /// Footnotes (`false`) and endnotes (`true`), by ID, with their part
    /// and its relationships.
    notes: HashMap<(bool, String), (Element, String, Relations)>,
    pictures: Pictures,
    /// Each picture part read, and the picture's hash if Docs keeps it.
    media: HashMap<String, Option<String>>,
    /// Each list's numbers so far, by its counter and level.
    counters: HashMap<(String, u8), u32>,
    fields: Vec<FieldState>,
    /// Each table of contents' look, in order.
    contents: Vec<ContentsStyle>,
    /// Whether the table of contents being read has dots before its page
    /// numbers.
    dotted: bool,
    /// Each font the text names, as named.
    fonts: BTreeSet<String>,
    lost: BTreeSet<Lost>,
    /// In a footnote, which has no pictures, footnotes, or page breaks.
    in_note: bool,
    /// The thread each comment's ID starts, or `None` for a reply.
    comment_threads: HashMap<String, Option<String>>,
    /// The threads whose start has been read.
    placed: HashSet<String>,
    /// Comment marks between paragraphs, for the next one.
    between: Vec<Inline>,
}

impl Reader<'_> {
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
            (Some(Ns::W), "p") => self.paragraph(element, context, out),
            (Some(Ns::W), "commentRangeStart" | "commentRangeEnd") => {
                let mut marks = Vec::new();
                self.comment_mark(element, &mut marks);
                self.between.extend(marks);
            }
            (Some(Ns::W), "tbl") => {
                if let Some(table) = self.table(element) {
                    out.push(Flat::plain(table));
                }
            }
            (Some(Ns::W), "sdt") => {
                let contents = element
                    .child(Ns::W, "sdtPr")
                    .and_then(|pr| pr.descendants(Ns::W, "docPartGallery").first().copied())
                    .and_then(Element::val)
                    == Some("Table of Contents");
                let before = self.contents.len();
                if let Some(content) = element.child(Ns::W, "sdtContent") {
                    for child in content.elements() {
                        self.block(child, context, out);
                    }
                }
                // A table of contents without Word's field around it.
                if contents && self.contents.len() == before {
                    self.contents.push(ContentsStyle::Plain);
                    out.push(Flat::plain(Block::Contents(ContentsStyle::Plain)));
                }
            }
            (Some(Ns::W), "customXml" | "ins" | "moveTo" | "smartTag") => {
                if matches!(element.name.as_str(), "ins" | "moveTo") {
                    self.lost.insert(Lost::TrackedChanges);
                }
                for child in element.elements() {
                    self.block(child, context, out);
                }
            }
            (Some(Ns::W), "del" | "moveFrom") => {
                self.lost.insert(Lost::TrackedChanges);
            }
            (Some(Ns::W), "altChunk") => {
                self.lost.insert(Lost::Shapes);
            }
            (Some(Ns::Mc), "AlternateContent") => {
                if let Some(chosen) = alternative(element) {
                    for child in chosen.elements() {
                        self.block(child, context, out);
                    }
                }
            }
            _ => {}
        }
    }

    /// The style ID a paragraph has, or the default one.
    fn para_style(&self, ppr: Option<&Element>) -> Option<String> {
        ppr.and_then(|ppr| ppr.child(Ns::W, "pStyle"))
            .and_then(Element::val)
            .map(str::to_owned)
            .or_else(|| self.styles.default_para.clone())
    }

    fn role(&self, style: Option<&str>, props: &ParaProps) -> Role {
        for style in self.styles.chain(style).iter().rev() {
            let name = style.name.as_str();
            if let Some(level) = name
                .strip_prefix("heading ")
                .and_then(|level| level.trim().parse::<u8>().ok())
                .filter(|level| (1..=9).contains(level))
            {
                return Role::Heading(level.min(6));
            }
            match name {
                "title" => return Role::Title,
                "subtitle" => return Role::Subtitle,
                "quote" | "intense quote" | "block quotation" | "quotations" | "block text" => {
                    return Role::Quote;
                }
                "code" | "html preformatted" | "source code" | "preformatted text"
                | "code block" => return Role::Code,
                // Word's heading over a table of contents isn't listed in it.
                "toc heading" => return Role::Normal,
                _ => {}
            }
        }
        match props.outline {
            Some(level @ 0..=5) => Role::Heading(level + 1),
            _ => Role::Normal,
        }
    }

    fn paragraph(&mut self, p: &Element, context: &Context, out: &mut Vec<Flat>) {
        let ppr = p.child(Ns::W, "pPr");
        let style = self.para_style(ppr);
        let chain = self.styles.chain(style.as_deref());
        let mut props = self.styles.para.clone();
        let mut base = self.styles.run.clone();
        if let Some((table_para, table_run)) = &context.table {
            props.over(table_para);
            base.over(table_run);
        }
        for style in &chain {
            props.over(&style.para);
            base.over(&style.run);
        }
        let direct = ParaProps::read(ppr);
        // The list the paragraph's in: its own, or its style's.
        let numbering = direct.numbering.clone().or(props.numbering.clone());
        let level = numbering
            .as_ref()
            .filter(|(id, _)| id != "0")
            .and_then(|(id, level)| Some((id.clone(), *level, self.level(id, *level)?)));
        if let Some((_, _, level)) = &level {
            props.over(&level.para);
        }
        props.over(&direct);
        let role = self.role(style.as_deref(), &props);

        // A table of contents' entries show its look.
        if self.in_contents()
            && let Some(tabs) = ppr.and_then(|ppr| ppr.child(Ns::W, "tabs"))
            && tabs
                .elements()
                .any(|tab| tab.attr(Ns::W, "leader") == Some("dot"))
        {
            self.dotted = true;
        }
        let hidden_before = self.suppressed();

        let mut list = None;
        let mut label = None;
        if let Some((id, number_level, level)) = level {
            match self.list_kind(&id, number_level, &level) {
                Some(kind) if matches!(role, Role::Normal | Role::Quote) => {
                    list = Some(ListRef {
                        level: number_level,
                        kind,
                    });
                }
                // A numbered heading keeps its number as text.
                Some(ListKind::Numbers(..)) => {
                    label = Some(self.label(&id, number_level, &level));
                }
                _ => {}
            }
        }

        let run_context = RunContext {
            base,
            role,
            link: None,
        };
        let mut pieces = Pieces {
            inlines: std::mem::take(&mut self.between),
            ..Pieces::default()
        };
        if let Some(label) = label.filter(|label| !label.is_empty()) {
            pieces
                .inlines
                .push(Inline::Text(format!("{label} "), Marks::default()));
        }
        self.inlines(&p.children, &run_context, &mut pieces);
        let Pieces {
            inlines,
            out: mut pieces_out,
            after,
            shown,
        } = pieces;
        pieces_out.push(Piece::Text(inlines));

        if self.suppressed() || hidden_before {
            // Inside a table of contents Docs makes again, or a field's
            // result it replaces: only the blocks it put in place.
            for piece in pieces_out {
                if let Piece::Block(block) = piece {
                    out.push(Flat::plain(block));
                }
            }
            out.extend(after);
            return;
        }

        // In a table, the table style's spacing is the cells' usual one.
        let cell_spacing = context
            .table
            .as_ref()
            .map(|(table, _)| (table.before, table.after));
        let para = self.para(&props, role, list.is_some(), cell_spacing);
        let in_list = list.is_some();
        let quote = role == Role::Quote && !in_list;
        let code = role == Role::Code && !in_list;
        if direct.break_before.or(props.break_before) == Some(true) && !self.in_note {
            out.push(Flat::plain(Block::PageBreak));
        }
        // An empty paragraph with only a line under it is a line.
        if !shown
            && pieces_out
                .iter()
                .all(|piece| matches!(piece, Piece::Text(inlines) if inlines.is_empty()))
            && let Some(rule) = ppr
                .and_then(|ppr| ppr.child(Ns::W, "pBdr"))
                .filter(|borders| borders.elements().all(|side| side.name == "bottom"))
                .and_then(|borders| borders.child(Ns::W, "bottom"))
                .map(border)
                .filter(|line| line.thickness > 0)
        {
            out.push(Flat::plain(Block::Rule(Rule {
                thickness: Some(rule.thickness).filter(|t| *t != 15),
                color: rule.color.filter(|color| color.hex_upper() != "C4C7C5"),
            })));
            return;
        }

        // Comment marks alone go with the text beside them.
        move_comment_marks(&mut pieces_out);
        // Empty text around a picture or a break isn't a line of its own.
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
            let is_break = matches!(block, Block::PageBreak);
            out.push(Flat {
                block,
                list: if is_break { None } else { list.clone() },
                more: !first && list.is_some(),
                quote: quote && !is_break,
                code: code && !is_break,
            });
            if !is_break {
                first = false;
            }
        }
        out.extend(after);
    }

    /// Where a comment's text starts or ends, or, for a comment marked
    /// only where it is, both there. Replies' marks are their thread's.
    fn comment_mark(&mut self, element: &Element, inlines: &mut Vec<Inline>) {
        if self.in_note {
            return;
        }
        let Some(Some(thread)) = element
            .attr(Ns::W, "id")
            .and_then(|id| self.comment_threads.get(id))
            .cloned()
        else {
            return;
        };
        match element.name.as_str() {
            "commentRangeStart" => {
                if self.placed.insert(thread.clone()) {
                    inlines.push(Inline::CommentStart(thread));
                }
            }
            "commentRangeEnd" => inlines.push(Inline::CommentEnd(thread)),
            _ => {
                if self.placed.insert(thread.clone()) {
                    inlines.push(Inline::CommentStart(thread.clone()));
                    inlines.push(Inline::CommentEnd(thread));
                }
            }
        }
    }

    /// A paragraph's layout in Docs' terms, leaving out what Docs gives it
    /// anyway.
    fn para(
        &self,
        props: &ParaProps,
        role: Role,
        in_list: bool,
        cell_spacing: Option<(Option<Twips>, Option<Twips>)>,
    ) -> Para {
        let (mut before, mut after) = role.spacing();
        if let Some((cell_before, cell_after)) = cell_spacing {
            before = cell_before.unwrap_or(before);
            after = cell_after.unwrap_or(after);
        }
        let size = self.styles.run.size.unwrap_or(22);
        let line = match props.line {
            Some(Spacing::Lines(line)) => Some((line * 100 / 240).max(1) as u16),
            Some(Spacing::Twips(twips)) => {
                let single = f64::from(size) / 2.0 * model::SINGLE;
                Some(((f64::from(twips) / 20.0 / single * 100.0).round().max(1.0)) as u16)
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
            // Docs keeps a list's items close together itself. Space a
            // file doesn't set is none, as Word shows it, rather than Docs'
            // own.
            before: Some(props.before.unwrap_or(0)).filter(|space| !in_list && *space != before),
            after: Some(props.after.unwrap_or(0)).filter(|space| !in_list && *space != after),
        }
    }

    /// The level of the list `id`, with the definition it uses.
    fn level(&self, id: &str, level: u8) -> Option<Level> {
        let (abstract_id, overrides) = self.numbering.nums.get(id)?;
        if let Some((_, Some(own))) = overrides.get(&level) {
            return Some(own.clone());
        }
        let mut abstract_id = abstract_id.clone();
        // A definition that's a numbering style's leads on to its list.
        for _ in 0..4 {
            let (levels, link) = self.numbering.abstracts.get(&abstract_id)?;
            if let Some(found) = levels.get(&level) {
                return Some(found.clone());
            }
            let link = link.as_deref()?;
            let (next, _) = self
                .styles
                .by_id
                .get(link)?
                .para
                .numbering
                .as_ref()
                .and_then(|(num, _)| self.numbering.nums.get(num))?;
            abstract_id = next.clone();
        }
        None
    }

    /// The counter a list's numbers go on, as Word counts them: lists
    /// using one definition carry on from each other, unless one starts
    /// again at its own number.
    fn counter(&self, id: &str, level: u8) -> String {
        match self.numbering.nums.get(id) {
            Some((_, overrides)) if overrides.get(&level).is_some_and(|(s, _)| s.is_some()) => {
                format!("num {id}")
            }
            Some((abstract_id, _)) => format!("abstract {abstract_id}"),
            None => format!("num {id}"),
        }
    }

    /// Counts an item of the list `id` at `level`, and returns its number.
    fn count(&mut self, id: &str, level: u8, item: &Level) -> u32 {
        let counter = self.counter(id, level);
        let start = self
            .numbering
            .nums
            .get(id)
            .and_then(|(_, overrides)| overrides.get(&level))
            .and_then(|(start, _)| *start)
            .unwrap_or(item.start);
        let number = self
            .counters
            .get(&(counter.clone(), level))
            .map_or(start, |last| last.saturating_add(1));
        self.counters.insert((counter.clone(), level), number);
        // Deeper levels start again after it.
        self.counters
            .retain(|(key, deeper), _| *key != counter || *deeper <= level);
        number
    }

    fn list_kind(&mut self, id: &str, level: u8, item: &Level) -> Option<ListKind> {
        let first = item.text.chars().next();
        match item.format.as_str() {
            "none" => None,
            "bullet" => Some(bullet_kind(first, level)),
            format => {
                let paren = item.text.trim_end().ends_with(')');
                let style = match format {
                    "lowerLetter" => ListStyle::LowerAlpha,
                    "upperLetter" => ListStyle::UpperAlpha,
                    "lowerRoman" => ListStyle::LowerRoman,
                    "upperRoman" => ListStyle::UpperRoman,
                    _ if paren => ListStyle::DecimalParen,
                    _ => ListStyle::Decimal,
                };
                let number = self.count(id, level, item);
                Some(ListKind::Numbers(
                    (style != ListStyle::Decimal).then_some(style),
                    number,
                ))
            }
        }
    }

    /// A numbered heading's number as Word shows it, such as "2.3".
    fn label(&mut self, id: &str, level: u8, item: &Level) -> String {
        let counter = self.counter(id, level);
        let mut text = item.text.clone();
        for shown in (0..=level).rev() {
            let value = self
                .counters
                .get(&(counter.clone(), shown))
                .copied()
                .unwrap_or(1);
            let format = self
                .level(id, shown)
                .map(|level| level.format)
                .unwrap_or_default();
            text = text.replace(&format!("%{}", shown + 1), &format_number(value, &format));
        }
        text
    }

    fn in_contents(&self) -> bool {
        self.fields
            .iter()
            .any(|field| field.action == Action::Contents)
    }

    /// Whether text read now is left out: a field's instruction, or a
    /// result Docs shows its own way.
    fn suppressed(&self) -> bool {
        self.fields.iter().any(|field| {
            !field.result || matches!(field.action, Action::Number(_) | Action::Contents)
        })
    }

    fn link(&self, context: &RunContext) -> Option<String> {
        self.fields
            .iter()
            .rev()
            .find_map(|field| match &field.action {
                Action::Link(link) if field.result => Some(link.clone()),
                _ => None,
            })
            .unwrap_or_else(|| context.link.clone())
    }

    fn inlines(&mut self, nodes: &[Node], context: &RunContext, pieces: &mut Pieces) {
        for node in nodes {
            let Node::Element(element) = node else {
                continue;
            };
            match (element.ns, element.name.as_str()) {
                (Some(Ns::W), "r") => self.run(element, context, pieces),
                (Some(Ns::W), "hyperlink") => {
                    let link = element
                        .attr(Ns::R, "id")
                        .and_then(|id| self.relations.get(id))
                        .filter(|relation| relation.external)
                        .and_then(|relation| model::safe_link(&relation.target));
                    let inner = RunContext {
                        link: link.or_else(|| context.link.clone()),
                        ..context.clone()
                    };
                    self.inlines(&element.children, &inner, pieces);
                }
                (Some(Ns::W), "fldSimple") => {
                    let instruction = element.attr(Ns::W, "instr").unwrap_or_default();
                    match action(instruction, &mut self.lost) {
                        Action::Number(field) => {
                            self.field_mark(field, element, context, pieces);
                        }
                        Action::Link(link) => {
                            let inner = RunContext {
                                link,
                                ..context.clone()
                            };
                            self.inlines(&element.children, &inner, pieces);
                        }
                        Action::Contents => {
                            self.contents.push(if instruction.contains("\\n") {
                                ContentsStyle::Links
                            } else {
                                ContentsStyle::Plain
                            });
                            pieces.block(Block::Contents(ContentsStyle::Plain));
                        }
                        Action::Keep => self.inlines(&element.children, context, pieces),
                    }
                }
                (Some(Ns::W), "ins" | "moveTo") => {
                    self.lost.insert(Lost::TrackedChanges);
                    self.inlines(&element.children, context, pieces);
                }
                (Some(Ns::W), "del" | "moveFrom") => {
                    self.lost.insert(Lost::TrackedChanges);
                }
                (Some(Ns::W), "smartTag" | "customXml" | "dir" | "bdo") => {
                    self.inlines(&element.children, context, pieces);
                }
                (Some(Ns::W), "sdt") => {
                    if let Some(content) = element.child(Ns::W, "sdtContent") {
                        self.inlines(&content.children, context, pieces);
                    }
                }
                (Some(Ns::W), "commentRangeStart" | "commentRangeEnd" | "commentReference") => {
                    self.comment_mark(element, &mut pieces.inlines);
                }
                (Some(Ns::M), "oMath" | "oMathPara") => {
                    self.lost.insert(Lost::Equations);
                    let text: String = element
                        .descendants(Ns::M, "t")
                        .iter()
                        .map(|t| t.text())
                        .collect();
                    if !text.is_empty() && !self.suppressed() {
                        pieces.shown = true;
                        let marks = self.marks(&context.base, context, None);
                        pieces.inlines.push(Inline::Text(text, marks));
                    }
                }
                (Some(Ns::Mc), "AlternateContent") => {
                    if let Some(chosen) = alternative(element) {
                        self.inlines(&chosen.children, context, pieces);
                    }
                }
                _ => {}
            }
        }
    }

    /// A page number or count, as the stand-in character Docs keeps its
    /// field on, with the look of the first run in the field's result.
    fn field_mark(
        &mut self,
        field: Field,
        element: &Element,
        context: &RunContext,
        pieces: &mut Pieces,
    ) {
        let props = element
            .descendants(Ns::W, "r")
            .first()
            .map(|run| self.run_props(run, context))
            .unwrap_or_else(|| context.base.clone());
        let mut marks = self.marks(&props, context, None);
        marks.field = Some(field);
        pieces.shown = true;
        pieces.inlines.push(Inline::Text("#".into(), marks));
    }

    /// A run's own properties over its character style's and the
    /// paragraph's.
    fn run_props(&self, run: &Element, context: &RunContext) -> RunProps {
        let rpr = run.child(Ns::W, "rPr");
        let mut props = context.base.clone();
        let style = rpr
            .and_then(|rpr| rpr.child(Ns::W, "rStyle"))
            .and_then(Element::val);
        // A link's look comes from the link itself in Docs.
        let in_link = self.link(context).is_some();
        if !(in_link && style == Some("Hyperlink")) {
            for style in self.styles.chain(style) {
                props.over(&style.run);
            }
        }
        props.over(&RunProps::read(rpr, &self.theme));
        props
    }

    /// Docs' marks for text with `props`.
    fn marks(&mut self, props: &RunProps, context: &RunContext, text: Option<&str>) -> Marks {
        let (size, color_default) = if self.in_note {
            (20, None)
        } else {
            context.role.text()
        };
        let color_default = color_default.and_then(Color::from_hex);
        // Docs' sixth heading is italic of itself.
        let italic_default = !self.in_note && context.role == Role::Heading(6);
        let mut marks = Marks {
            bold: props.bold == Some(true),
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
            highlight: props.highlight.flatten().or(props.shading.flatten()),
            size: props.size.filter(|own| *own != size),
            font: props.font.clone(),
            link: self.link(context),
            ..Marks::default()
        };
        if marks.link.is_some() {
            marks.underline = false;
            if marks.color.is_some_and(|color| {
                matches!(color.hex_upper().as_str(), "0563C1" | "1155CC" | "0000FF")
            }) {
                marks.color = None;
            }
        }
        if let Some(font) = &props.font
            && text.is_some_and(|text| !text.trim().is_empty())
        {
            self.fonts.insert(font.clone());
        }
        // Docs' own code marks, as it writes them in Word.
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

    fn text(&mut self, text: &str, props: &RunProps, context: &RunContext, pieces: &mut Pieces) {
        if text.is_empty() || self.suppressed() {
            return;
        }
        pieces.shown = true;
        let text = text.replace(['\n', '\r'], " ");
        let text = if props.caps == Some(true) {
            text.to_uppercase()
        } else {
            text
        };
        let marks = self.marks(props, context, Some(&text));
        pieces.inlines.push(Inline::Text(text, marks));
    }

    fn run(&mut self, run: &Element, context: &RunContext, pieces: &mut Pieces) {
        let props = self.run_props(run, context);
        let hidden = props.hidden == Some(true);
        for element in run.elements() {
            if element.ns == Some(Ns::Mc) && element.name == "AlternateContent" {
                // Its choice holds what a run would.
                if let Some(chosen) = alternative(element) {
                    for child in chosen.elements() {
                        self.run_part(child, &props, hidden, context, pieces);
                    }
                }
                continue;
            }
            self.run_part(element, &props, hidden, context, pieces);
        }
    }

    fn run_part(
        &mut self,
        element: &Element,
        props: &RunProps,
        hidden: bool,
        context: &RunContext,
        pieces: &mut Pieces,
    ) {
        if element.ns != Some(Ns::W) {
            return;
        }
        match element.name.as_str() {
            "fldChar" => match element.attr(Ns::W, "fldCharType") {
                Some("begin") => self.fields.push(FieldState {
                    instruction: String::new(),
                    result: false,
                    action: Action::Keep,
                }),
                Some("separate") => {
                    let mut lost = std::mem::take(&mut self.lost);
                    if let Some(field) = self.fields.last_mut()
                        && !field.result
                    {
                        field.result = true;
                        field.action = action(&field.instruction, &mut lost);
                    }
                    self.lost = lost;
                    self.field_started(props, context, pieces);
                }
                Some("end") => {
                    let mut lost = std::mem::take(&mut self.lost);
                    let unfinished =
                        self.fields
                            .last_mut()
                            .filter(|field| !field.result)
                            .map(|field| {
                                field.result = true;
                                field.action = action(&field.instruction, &mut lost);
                            });
                    self.lost = lost;
                    if unfinished.is_some() {
                        self.field_started(props, context, pieces);
                    }
                    if let Some(field) = self.fields.pop()
                        && field.action == Action::Contents
                    {
                        let dotted = std::mem::replace(&mut self.dotted, false);
                        let style = if field.instruction.contains("\\n") {
                            ContentsStyle::Links
                        } else if dotted {
                            ContentsStyle::Dotted
                        } else {
                            ContentsStyle::Plain
                        };
                        if let Some(last) = self.contents.last_mut() {
                            *last = style;
                        }
                    }
                }
                _ => {}
            },
            "instrText" => {
                if let Some(field) = self.fields.last_mut()
                    && !field.result
                {
                    field.instruction.push_str(&element.text());
                }
            }
            _ if hidden => {}
            "t" => self.text(&element.text(), props, context, pieces),
            "tab" | "ptab" => {
                if !self.suppressed() {
                    self.lost.insert(Lost::Tabs);
                }
                self.text(" ", props, context, pieces);
            }
            "noBreakHyphen" => self.text("\u{2011}", props, context, pieces),
            "softHyphen" => self.text("\u{ad}", props, context, pieces),
            "sym" => {
                let symbol_font = element.attr(Ns::W, "font").is_some_and(|font| {
                    let font = font.to_ascii_lowercase();
                    font.contains("symbol")
                        || font.contains("wingdings")
                        || font.contains("webdings")
                });
                let character = element
                    .attr(Ns::W, "char")
                    .and_then(|code| u32::from_str_radix(code, 16).ok())
                    .and_then(char::from_u32);
                if let Some(character) = character.filter(|_| !symbol_font) {
                    self.text(&character.to_string(), props, context, pieces);
                }
            }
            "br" | "cr" => {
                if self.suppressed() {
                    return;
                }
                match element.attr(Ns::W, "type") {
                    Some("page") if !self.in_note => pieces.block(Block::PageBreak),
                    Some("page" | "column") => {}
                    _ => {
                        pieces.shown = true;
                        pieces.inlines.push(Inline::Break);
                    }
                }
            }
            "drawing" | "pict" | "object" if self.in_note => {
                self.lost.insert(Lost::Pictures);
            }
            "drawing" => {
                if !self.suppressed() {
                    self.drawing(element, pieces);
                }
            }
            "pict" | "object" => {
                if !self.suppressed() {
                    self.vml(element, pieces);
                }
            }
            "footnoteReference" | "endnoteReference" => {
                if self.suppressed() || self.in_note {
                    return;
                }
                let endnote = element.name == "endnoteReference";
                if endnote {
                    self.lost.insert(Lost::Endnotes);
                }
                if let Some(id) = element.attr(Ns::W, "id")
                    && let Some(text) = self.note(endnote, id)
                {
                    pieces.shown = true;
                    pieces.inlines.push(Inline::Footnote(text));
                }
            }
            "commentReference" => self.comment_mark(element, &mut pieces.inlines),
            "ruby" => {
                if let Some(base) = element.child(Ns::W, "rubyBase") {
                    for run in base.elements().filter(|e| e.is(Ns::W, "r")) {
                        self.run(run, context, pieces);
                    }
                }
            }
            _ => {}
        }
    }

    /// After a field's instruction: Docs' own page number or table of
    /// contents in place of its result.
    fn field_started(&mut self, props: &RunProps, context: &RunContext, pieces: &mut Pieces) {
        let Some(field) = self.fields.last() else {
            return;
        };
        // Only the outermost field Docs replaces counts.
        let outer = self.fields[..self.fields.len() - 1].iter().any(|field| {
            matches!(field.action, Action::Number(_) | Action::Contents) || !field.result
        });
        if outer {
            return;
        }
        match field.action {
            Action::Number(kind) => {
                let mut marks = self.marks(props, context, None);
                marks.field = Some(kind);
                pieces.shown = true;
                pieces.inlines.push(Inline::Text("#".into(), marks));
            }
            Action::Contents => {
                self.dotted = false;
                self.contents.push(ContentsStyle::Plain);
                pieces.block(Block::Contents(ContentsStyle::Plain));
            }
            _ => {}
        }
    }

    /// A footnote's or endnote's text, its paragraphs joined by line breaks.
    fn note(&mut self, endnote: bool, id: &str) -> Option<Vec<Inline>> {
        let (note, part, relations) = self.notes.get(&(endnote, id.to_owned()))?;
        // Read from a copy, since reading needs the reader.
        let paragraphs: Vec<Element> = note
            .elements()
            .filter(|element| element.is(Ns::W, "p"))
            .cloned()
            .collect();
        let saved = (
            std::mem::replace(&mut self.part, part.clone()),
            std::mem::replace(&mut self.relations, relations.clone()),
            std::mem::replace(&mut self.in_note, true),
            std::mem::take(&mut self.fields),
        );
        let mut text = Vec::new();
        for paragraph in &paragraphs {
            let ppr = paragraph.child(Ns::W, "pPr");
            let style = self.para_style(ppr);
            let mut base = self.styles.run.clone();
            for style in self.styles.chain(style.as_deref()) {
                base.over(&style.run);
            }
            let context = RunContext {
                base,
                role: Role::Normal,
                link: None,
            };
            let mut pieces = Pieces::default();
            self.inlines(&paragraph.children, &context, &mut pieces);
            let mut inlines = pieces.inlines;
            if !text.is_empty() {
                text.push(Inline::Break);
            } else if let Some(Inline::Text(first, _)) = inlines.first_mut() {
                // Word puts a space after the note's number.
                *first = first.trim_start().to_owned();
            }
            text.append(&mut inlines);
        }
        (self.part, self.relations, self.in_note, self.fields) = saved;
        Some(text)
    }

    /// The picture a relationship leads to, by its hash, if Docs keeps it.
    fn picture(&mut self, id: &str) -> Option<String> {
        let relation = self.relations.get(id)?.clone();
        if relation.external {
            self.lost.insert(Lost::Pictures);
            return None;
        }
        if let Some(known) = self.media.get(&relation.target) {
            return known.clone();
        }
        let found = self
            .package
            .bytes(&relation.target, MAX_PICTURE)
            .and_then(Picture::read)
            .map(|picture| {
                let hash = picture.hash.clone();
                self.pictures.insert(hash.clone(), picture);
                hash
            });
        if found.is_none() {
            self.lost.insert(Lost::Pictures);
        }
        self.media.insert(relation.target, found.clone());
        found
    }

    /// A DrawingML drawing: a picture, a text box, or a shape.
    fn drawing(&mut self, drawing: &Element, pieces: &mut Pieces) {
        for place in drawing.elements() {
            if !place.is(Ns::Wp, "inline") && !place.is(Ns::Wp, "anchor") {
                continue;
            }
            let width = place
                .child(Ns::Wp, "extent")
                .and_then(|extent| extent.attr(Ns::None, "cx"))
                .and_then(|cx| cx.parse::<f64>().ok())
                // English Metric Units: 9,525 to the pixel.
                .map(|cx| (cx / 9525.0).round().clamp(1.0, 5000.0) as u32);
            let alt = place
                .child(Ns::Wp, "docPr")
                .and_then(|pr| {
                    pr.attr(Ns::None, "descr")
                        .filter(|alt| !alt.trim().is_empty())
                        .or_else(|| pr.attr(Ns::None, "title"))
                })
                .unwrap_or_default()
                .trim()
                .to_owned();
            let Some(data) = place
                .child(Ns::A, "graphic")
                .and_then(|graphic| graphic.child(Ns::A, "graphicData"))
            else {
                continue;
            };
            let pictures = data.descendants(Ns::Pic, "pic");
            let boxes = data.descendants(Ns::W, "txbxContent");
            if pictures.is_empty() && boxes.is_empty() {
                self.lost.insert(Lost::Shapes);
                continue;
            }
            if data.child(Ns::Pic, "pic").is_none() && !pictures.is_empty() {
                self.lost.insert(Lost::Shapes);
            }
            for picture in pictures {
                let Some(id) = picture.descendants(Ns::A, "blip").first().and_then(|blip| {
                    blip.attr(Ns::R, "embed")
                        .or_else(|| blip.attr(Ns::R, "link"))
                }) else {
                    continue;
                };
                if let Some(hash) = self.picture(id) {
                    pieces.block(Block::Image(Image {
                        hash,
                        alt: alt.clone(),
                        width,
                    }));
                }
            }
            self.text_boxes(&boxes, pieces);
        }
    }

    /// An older VML picture, text box, or shape, or an embedded object.
    fn vml(&mut self, element: &Element, pieces: &mut Pieces) {
        let images = element.descendants(Ns::V, "imagedata");
        let boxes = element.descendants(Ns::W, "txbxContent");
        let mut kept = false;
        for image in images {
            let Some(id) = image.attr(Ns::R, "id") else {
                continue;
            };
            if let Some(hash) = self.picture(id) {
                kept = true;
                pieces.block(Block::Image(Image {
                    hash,
                    alt: image
                        .attr(Ns::Other, "title")
                        .unwrap_or_default()
                        .to_owned(),
                    width: None,
                }));
            }
        }
        if !kept && boxes.is_empty() {
            self.lost.insert(Lost::Shapes);
        }
        self.text_boxes(&boxes, pieces);
    }

    /// Text boxes' blocks, which go after the paragraph they're in.
    fn text_boxes(&mut self, boxes: &[&Element], pieces: &mut Pieces) {
        if boxes.is_empty() || self.in_note {
            return;
        }
        self.lost.insert(Lost::TextBoxes);
        for content in boxes {
            let flats = self.container(&content.children, &Context::default());
            pieces.after.extend(flats);
            pieces.shown = true;
        }
    }

    fn table(&mut self, table: &Element) -> Option<Block> {
        let tblpr = table.child(Ns::W, "tblPr");
        let style = tblpr
            .and_then(|tblpr| tblpr.child(Ns::W, "tblStyle"))
            .and_then(Element::val);
        let chain = self.styles.chain(style);
        let mut borders = Borders::default();
        let mut table_para = ParaProps::default();
        let mut table_run = RunProps::default();
        for style in &chain {
            borders.over(&style.borders);
            table_para.over(&style.para);
            table_run.over(&style.run);
        }
        borders.over(&Borders::read(
            tblpr.and_then(|tblpr| tblpr.child(Ns::W, "tblBorders")),
        ));
        let margins = tblpr
            .and_then(|tblpr| tblpr.child(Ns::W, "tblCellMar"))
            .map(padding);
        let widths: Vec<u32> = table
            .child(Ns::W, "tblGrid")
            .map(|grid| {
                grid.elements()
                    .filter(|col| col.is(Ns::W, "gridCol"))
                    .map(|col| {
                        number::<f64>(Some(col), "w")
                            // Twips to pixels, at 96 to the inch.
                            .map_or(0, |w| (w / 15.0).round().clamp(0.0, 5000.0) as u32)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let context = Context {
            table: Some((table_para, table_run)),
        };

        // Each cell, with its column, and the cell above it in each
        // column that a merge carries on.
        struct Placed {
            cell: Cell,
            column: usize,
            span: usize,
            row: usize,
            borders: [Option<Border>; 4],
        }
        let mut placed: Vec<Placed> = Vec::new();
        let mut rows: Vec<(Option<Twips>, bool)> = Vec::new();
        let mut above: HashMap<usize, usize> = HashMap::new();
        for tr in rows_of(table) {
            let row = rows.len();
            let trpr = tr.child(Ns::W, "trPr");
            let height = trpr
                .and_then(|trpr| trpr.child(Ns::W, "trHeight"))
                .and_then(|height| number::<f64>(Some(height), "val"))
                .map(|height| (height.round() as Twips).clamp(0, model::MAX_ROW_HEIGHT))
                .filter(|height| *height > 0);
            let header = on(trpr.and_then(|trpr| trpr.child(Ns::W, "tblHeader")));
            rows.push((height, header));
            let mut column: usize = trpr
                .and_then(|trpr| trpr.child(Ns::W, "gridBefore"))
                .and_then(|before| number(Some(before), "val"))
                .unwrap_or(0);
            for tc in cells_of(tr) {
                let tcpr = tc.child(Ns::W, "tcPr");
                let span: usize = tcpr
                    .and_then(|tcpr| tcpr.child(Ns::W, "gridSpan"))
                    .and_then(|span| number(Some(span), "val"))
                    .unwrap_or(1usize)
                    .clamp(1, model::MAX_SPAN as usize);
                let merge = tcpr.and_then(|tcpr| tcpr.child(Ns::W, "vMerge"));
                let carries_on = merge.is_some_and(|merge| merge.val() != Some("restart"));
                if carries_on && let Some(&origin) = above.get(&column) {
                    placed[origin].cell.rowspan += 1;
                    column += span;
                    continue;
                }
                let mut sides: [Option<Border>; 4] = [None; 4];
                if let Some(own) = tcpr.and_then(|tcpr| tcpr.child(Ns::W, "tcBorders")) {
                    let own = Borders::read(Some(own));
                    sides = [own.top, own.right, own.bottom, own.left];
                }
                let background = tcpr
                    .and_then(|tcpr| tcpr.child(Ns::W, "shd"))
                    .and_then(|shd| color(shd.attr(Ns::W, "fill")))
                    .filter(|fill| fill.hex_upper() != "FFFFFF")
                    .filter(|fill| !(header && fill.hex_upper() == "F1F3F4"));
                let cell_padding = tcpr
                    .and_then(|tcpr| tcpr.child(Ns::W, "tcMar"))
                    .map(padding)
                    .or(margins);
                let flats = self.container(&tc.children, &context);
                let mut content = group(flats);
                // Docs shows a heading cell's text bold itself.
                if header {
                    unbold(&mut content);
                }
                let colwidth = (column + span <= widths.len()
                    && widths[column..column + span].iter().all(|w| *w > 0))
                .then(|| widths[column..column + span].to_vec());
                let index = placed.len();
                for each in column..column + span {
                    above.remove(&each);
                }
                if merge.is_some() {
                    above.insert(column, index);
                }
                placed.push(Placed {
                    cell: Cell {
                        header,
                        colspan: span as u32,
                        rowspan: 1,
                        colwidth,
                        background,
                        borders: [None; 4],
                        border_color: None,
                        padding: cell_padding,
                        content,
                    },
                    column,
                    span,
                    row,
                    borders: sides,
                });
                column += span;
            }
            // Merges carry on only into the next row.
            let reached: Vec<usize> = placed
                .iter()
                .enumerate()
                .filter(|(_, p)| p.row + p.cell.rowspan as usize > row)
                .map(|(index, _)| index)
                .collect();
            above.retain(|_, origin| reached.contains(origin));
        }
        if rows.is_empty() {
            return None;
        }
        let columns = placed
            .iter()
            .map(|p| p.column + p.span)
            .max()
            .unwrap_or(0)
            .max(widths.len());
        let last_row = rows.len() - 1;
        let mut out: Vec<Row> = rows
            .iter()
            .map(|(height, _)| Row {
                height: *height,
                cells: Vec::new(),
            })
            .collect();
        for mut p in placed {
            let bottom_row = p.row + p.cell.rowspan as usize - 1;
            // Each side: the cell's own, or the table's edge or inside line.
            let table_side = [
                if p.row == 0 {
                    borders.top
                } else {
                    borders.inside_h
                },
                if p.column + p.span >= columns {
                    borders.right
                } else {
                    borders.inside_v
                },
                if bottom_row >= last_row {
                    borders.bottom
                } else {
                    borders.inside_h
                },
                if p.column == 0 {
                    borders.left
                } else {
                    borders.inside_v
                },
            ];
            let mut sides = [None; 4];
            let mut colors = Vec::new();
            for at in 0..4 {
                let side = p.borders[at].or(table_side[at]);
                sides[at] = Some(side.map_or(0, |side| side.thickness));
                if let Some(side) = side
                    && side.thickness > 0
                {
                    colors.push(side.color.unwrap_or(Color::BLACK));
                }
            }
            let color = colors.first().copied();
            // Docs' own thin gray lines, as it writes them in Word.
            let usual = sides.iter().all(|side| *side == Some(10))
                && colors.iter().all(|c| c.hex_upper() == "C4C7C5");
            if usual {
                p.cell.borders = [None; 4];
                p.cell.border_color = None;
            } else {
                p.cell.borders = sides;
                p.cell.border_color = color.filter(|c| *c != Color::BLACK);
            }
            out[p.row].cells.push(p.cell);
        }
        Some(Block::Table(out))
    }
}

/// Cell margins, as `[top, right, bottom, left]` in twips.
fn padding(margins: &Element) -> [Twips; 4] {
    let side = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| margins.child(Ns::W, name))
            .and_then(|side| number::<f64>(Some(side), "w"))
            .map_or(0, |w| (w.round() as Twips).clamp(0, model::MAX_PADDING))
    };
    [
        side(&["top"]),
        side(&["right", "end"]),
        side(&["bottom"]),
        side(&["left", "start"]),
    ]
}

/// A table's rows, inside any content controls around them.
fn rows_of(table: &Element) -> Vec<&Element> {
    let mut out = Vec::new();
    for element in table.elements() {
        if element.is(Ns::W, "tr") {
            out.push(element);
        } else if element.is(Ns::W, "sdt") || element.is(Ns::W, "customXml") {
            let inner = element.child(Ns::W, "sdtContent").unwrap_or(element);
            out.extend(rows_of(inner));
        }
    }
    out
}

/// A row's cells, inside any content controls around them.
fn cells_of(row: &Element) -> Vec<&Element> {
    let mut out = Vec::new();
    for element in row.elements() {
        if element.is(Ns::W, "tc") {
            out.push(element);
        } else if element.is(Ns::W, "sdt") || element.is(Ns::W, "customXml") {
            let inner = element.child(Ns::W, "sdtContent").unwrap_or(element);
            out.extend(cells_of(inner));
        }
    }
    out
}

/// The part of `mc:AlternateContent` to read: its first choice, which is
/// what Word itself shows, or its fallback.
fn alternative(element: &Element) -> Option<&Element> {
    element
        .child(Ns::Mc, "Choice")
        .or_else(|| element.child(Ns::Mc, "Fallback"))
}

#[cfg(test)]
mod tests {
    use super::flat::plain;
    use super::*;
    use crate::docx;
    use crate::model::Cell;

    fn text(words: &str) -> Inline {
        Inline::Text(words.to_owned(), Marks::default())
    }

    fn para(words: &str) -> Block {
        Block::Paragraph(Para::default(), vec![text(words)])
    }

    fn png(seed: u8) -> Picture {
        // A 1 by 1 PNG whose color differs by `seed`.
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
        let file = docx::write(blocks, parts, Some(setup), "Test", pictures, &[], &[]);
        let mut report = Report::default();
        let mut read = read(&file, &mut report).expect("Docs reads its own Word files");
        read.blocks = model::normalize(read.blocks);
        read.parts = read.parts.normalized();
        (read, report)
    }

    #[test]
    fn comments_come_back_from_docs_word_files() {
        let (blocks, threads) = super::super::comment_fixture::document();
        let file = docx::write(
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
    fn reads_comments_as_google_docs_and_word_write_them() {
        // Google Docs: times in UTC. Word: local times in `w:date`, with
        // UTC ones by durable ID, and replies through commentsExtended.
        let document = format!(
            "<w:document {W} xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\"><w:body>\
             <w:commentRangeStart w:id=\"5\"/>\
             <w:p><w:r><w:t>Plan</w:t></w:r><w:commentRangeEnd w:id=\"5\"/><w:r><w:commentReference w:id=\"5\"/></w:r>\
             <w:commentRangeStart w:id=\"6\"/><w:r><w:t xml:space=\"preserve\"> for </w:t></w:r>\
             <w:commentRangeStart w:id=\"7\"/><w:r><w:t>today</w:t></w:r><w:commentRangeEnd w:id=\"7\"/>\
             <w:commentRangeEnd w:id=\"6\"/><w:r><w:commentReference w:id=\"6\"/></w:r><w:r><w:commentReference w:id=\"7\"/></w:r></w:p>\
             </w:body></w:document>"
        );
        let comments = "<w:comments xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
             xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\">\
             <w:comment w:id=\"5\" w:author=\"Project Spectacles\" w:date=\"2026-11-24T07:45:18Z\">\
             <w:p w14:paraId=\"00000022\"><w:r><w:t>On the first line &quot;Plan&quot;</w:t></w:r></w:p></w:comment>\
             <w:comment w:id=\"6\" w:author=\"Dana Lee\" w:date=\"2026-11-24T09:00:00Z\">\
             <w:p w14:paraId=\"1A2B3C4D\"><w:r><w:annotationRef/></w:r><w:r><w:t>Which day?</w:t></w:r></w:p></w:comment>\
             <w:comment w:id=\"7\" w:author=\"Sam Kim\" w:date=\"2026-11-24T10:00:00Z\">\
             <w:p><w:r><w:t>Today</w:t></w:r></w:p><w:p w14:paraId=\"5E6F7A8B\"><w:r><w:t>at noon</w:t></w:r></w:p></w:comment>\
             </w:comments>";
        let extended = "<w15:commentsEx xmlns:w15=\"http://schemas.microsoft.com/office/word/2012/wordml\">\
             <w15:commentEx w15:paraId=\"00000022\" w15:done=\"0\"/>\
             <w15:commentEx w15:paraId=\"1A2B3C4D\" w15:done=\"1\"/>\
             <w15:commentEx w15:paraId=\"5E6F7A8B\" w15:paraIdParent=\"1a2b3c4d\" w15:done=\"0\"/></w15:commentsEx>";
        let ids = "<w16cid:commentsIds xmlns:w16cid=\"http://schemas.microsoft.com/office/word/2016/wordml/cid\">\
             <w16cid:commentId w16cid:paraId=\"5E6F7A8B\" w16cid:durableId=\"0F1E2D3C\"/></w16cid:commentsIds>";
        let extensible = "<w16cex:commentsExtensible xmlns:w16cex=\"http://schemas.microsoft.com/office/word/2018/wordml/cex\">\
             <w16cex:commentExtensible w16cex:durableId=\"0F1E2D3C\" w16cex:dateUtc=\"2026-11-24T15:00:00Z\"/></w16cex:commentsExtensible>";
        let file = word_file(
            &document,
            &[
                ("word/comments.xml", comments),
                ("word/commentsExtended.xml", extended),
                ("word/commentsIds.xml", ids),
                ("word/commentsExtensible.xml", extensible),
            ],
        );
        let mut report = Report::default();
        let read = read(&file, &mut report).unwrap();
        assert!(report.lost.is_empty(), "{report:?}");
        assert_eq!(read.comments.len(), 2);
        let first = &read.comments[0];
        assert_eq!(first.comments[0].name, "Project Spectacles");
        assert_eq!(first.comments[0].text, "On the first line \"Plan\"");
        assert!(first.resolved.is_none());
        let second = &read.comments[1];
        assert!(second.resolved.is_some());
        assert_eq!(second.comments.len(), 2);
        assert_eq!(second.comments[1].text, "Today\nat noon");
        assert_eq!(second.comments[1].at, "2026-11-24T15:00:00Z");
        let quotes = model::comment_quotes(&read.blocks);
        assert_eq!(quotes[&first.id], "Plan");
        // The reply's own marks are left out: the thread is about the
        // first comment's text.
        assert_eq!(quotes[&second.id], "for today");
        assert_eq!(quotes.len(), 2);
    }

    #[test]
    fn docs_word_files_come_back_the_same() {
        let picture = png(7);
        let mut pictures = Pictures::new();
        pictures.insert(picture.hash.clone(), picture.clone());
        // Word has no Light that's bold, so Docs writes the weight it shows.
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
                    text("Some "),
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
            Block::Code("let a = 1;\nlet b = 2;".into()),
            Block::Rule(Rule {
                thickness: Some(40),
                color: Some(Color([0, 0, 0xff])),
            }),
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
            margins: [36.0, 54.0, 36.0, 54.0],
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
        // Lato is Docs' own, so no font stands in.
        assert_eq!(report.fonts, Vec::new());
    }

    /// A Word file of `parts`, each a path and its XML, as Word lays one
    /// out.
    fn word_file(document: &str, extra: &[(&str, &str)]) -> Vec<u8> {
        use std::io::Write as _;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        let mut relations = String::from(
            "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
        );
        for (path, _) in extra {
            let Some(name) = path.strip_prefix("word/") else {
                continue;
            };
            let kind = name
                .trim_end_matches(".xml")
                .trim_end_matches(char::is_numeric);
            let kind = match kind {
                "theme/theme" => "theme",
                "header" | "footer" | "styles" | "numbering" | "footnotes" | "endnotes"
                | "settings" | "comments" | "commentsExtended" | "commentsIds"
                | "commentsExtensible" => kind,
                _ => continue,
            };
            relations.push_str(&format!(
                "<Relationship Id=\"r{kind}{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}\" Target=\"{name}\"/>",
                name.len()
            ));
        }
        relations.push_str("<Relationship Id=\"rLink\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink\" Target=\"https://example.com/a?b=1&amp;c=2\" TargetMode=\"External\"/></Relationships>");
        let package = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>";
        let mut files = vec![
            ("_rels/.rels", package.to_owned()),
            ("word/_rels/document.xml.rels", relations),
            ("word/document.xml", document.to_owned()),
        ];
        files.extend(extra.iter().map(|(path, xml)| (*path, (*xml).to_owned())));
        for (path, xml) in files {
            zip.start_file(path, options).unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:wps=\"http://schemas.microsoft.com/office/word/2010/wordprocessingShape\" xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\" xmlns:m=\"http://schemas.openxmlformats.org/officeDocument/2006/math\"";

    #[test]
    fn space_a_word_file_doesnt_set_is_none() {
        let document = format!(
            "<w:document {W}><w:body><w:p><w:r><w:t>Close</w:t></w:r></w:p>\
             <w:p><w:pPr><w:spacing w:after=\"120\"/></w:pPr><w:r><w:t>Usual</w:t></w:r></w:p></w:body></w:document>"
        );
        let mut report = Report::default();
        let read = read(&word_file(&document, &[]), &mut report).unwrap();
        let Block::Paragraph(close, _) = &read.blocks[0] else {
            panic!("{:?}", read.blocks);
        };
        assert_eq!((close.before, close.after), (None, Some(0)));
        let Block::Paragraph(usual, _) = &read.blocks[1] else {
            panic!("{:?}", read.blocks);
        };
        assert_eq!(usual.after, None);
    }

    #[test]
    fn reads_word_files_as_word_writes_them() {
        let styles = format!(
            "<w:styles {W}><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:asciiTheme=\"minorHAnsi\" w:hAnsiTheme=\"minorHAnsi\"/><w:sz w:val=\"22\"/></w:rPr></w:rPrDefault>\
             <w:pPrDefault><w:pPr><w:spacing w:after=\"160\" w:line=\"259\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults>\
             <w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/></w:style>\
             <w:style w:type=\"paragraph\" w:styleId=\"Heading1\"><w:name w:val=\"heading 1\"/><w:basedOn w:val=\"Normal\"/><w:pPr><w:spacing w:before=\"240\" w:after=\"0\"/><w:outlineLvl w:val=\"0\"/></w:pPr><w:rPr><w:rFonts w:asciiTheme=\"majorHAnsi\" w:hAnsiTheme=\"majorHAnsi\"/><w:color w:val=\"2F5496\" w:themeColor=\"accent1\"/><w:sz w:val=\"32\"/></w:rPr></w:style>\
             <w:style w:type=\"paragraph\" w:styleId=\"MyHeading\"><w:name w:val=\"My Heading\"/><w:basedOn w:val=\"Heading1\"/><w:rPr><w:b/></w:rPr></w:style>\
             <w:style w:type=\"table\" w:styleId=\"TableGrid\"><w:name w:val=\"Table Grid\"/><w:pPr><w:spacing w:after=\"0\"/></w:pPr><w:tblPr><w:tblBorders><w:top w:val=\"single\" w:sz=\"4\" w:color=\"auto\"/><w:left w:val=\"single\" w:sz=\"4\" w:color=\"auto\"/><w:bottom w:val=\"single\" w:sz=\"4\" w:color=\"auto\"/><w:right w:val=\"single\" w:sz=\"4\" w:color=\"auto\"/><w:insideH w:val=\"single\" w:sz=\"4\" w:color=\"auto\"/><w:insideV w:val=\"single\" w:sz=\"4\" w:color=\"auto\"/></w:tblBorders></w:tblPr></w:style></w:styles>"
        );
        let theme = "<a:theme xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><a:themeElements><a:fontScheme name=\"Office\"><a:majorFont><a:latin typeface=\"Calibri Light\"/></a:majorFont><a:minorFont><a:latin typeface=\"Calibri\"/></a:minorFont></a:fontScheme></a:themeElements></a:theme>";
        let numbering = format!(
            "<w:numbering {W}><w:abstractNum w:abstractNumId=\"0\"><w:lvl w:ilvl=\"0\"><w:start w:val=\"1\"/><w:numFmt w:val=\"decimal\"/><w:lvlText w:val=\"%1.\"/></w:lvl><w:lvl w:ilvl=\"1\"><w:start w:val=\"1\"/><w:numFmt w:val=\"lowerLetter\"/><w:lvlText w:val=\"%1.%2\"/></w:lvl></w:abstractNum>\
             <w:abstractNum w:abstractNumId=\"1\"><w:lvl w:ilvl=\"0\"><w:start w:val=\"1\"/><w:numFmt w:val=\"bullet\"/><w:lvlText w:val=\"&#xF0B7;\"/><w:rPr><w:rFonts w:ascii=\"Symbol\"/></w:rPr></w:lvl></w:abstractNum>\
             <w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num><w:num w:numId=\"2\"><w:abstractNumId w:val=\"0\"/></w:num><w:num w:numId=\"3\"><w:abstractNumId w:val=\"1\"/></w:num></w:numbering>"
        );
        let footer = format!(
            "<w:ftr {W}><w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr><w:r><w:t xml:space=\"preserve\">Page </w:t></w:r><w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText xml:space=\"preserve\"> PAGE   \\* MERGEFORMAT </w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:rPr><w:noProof/></w:rPr><w:t>2</w:t></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p></w:ftr>"
        );
        let item = |num: u32, level: u32, words: &str| {
            format!(
                "<w:p><w:pPr><w:numPr><w:ilvl w:val=\"{level}\"/><w:numId w:val=\"{num}\"/></w:numPr></w:pPr><w:r><w:t>{words}</w:t></w:r></w:p>"
            )
        };
        let document = format!(
            "<w:document {W}><w:body>\
             <w:sdt><w:sdtPr><w:docPartObj><w:docPartGallery w:val=\"Table of Contents\"/></w:docPartObj></w:sdtPr><w:sdtContent>\
             <w:p><w:r><w:t>Contents</w:t></w:r></w:p>\
             <w:p><w:pPr><w:tabs><w:tab w:val=\"right\" w:leader=\"dot\" w:pos=\"9350\"/></w:tabs></w:pPr><w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText> TOC \\o \"1-3\" \\h \\z \\u </w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:hyperlink w:anchor=\"_Toc1\"><w:r><w:t>Intro</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>1</w:t></w:r></w:hyperlink></w:p>\
             <w:p><w:pPr><w:tabs><w:tab w:val=\"right\" w:leader=\"dot\" w:pos=\"9350\"/></w:tabs></w:pPr><w:hyperlink w:anchor=\"_Toc2\"><w:r><w:t>Plans</w:t></w:r></w:hyperlink><w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p>\
             </w:sdtContent></w:sdt>\
             <w:p><w:pPr><w:pStyle w:val=\"Heading1\"/><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/></w:numPr></w:pPr><w:r><w:t>Intro</w:t></w:r></w:p>\
             <w:p><w:pPr><w:pStyle w:val=\"MyHeading\"/></w:pPr><w:r><w:t>Plans</w:t></w:r></w:p>\
             <w:p><w:r><w:t xml:space=\"preserve\">Kept </w:t></w:r><w:ins w:id=\"1\"><w:r><w:t>added</w:t></w:r></w:ins><w:del w:id=\"2\"><w:r><w:delText>gone</w:delText></w:r></w:del><w:commentRangeStart w:id=\"0\"/><w:r><w:tab/><w:t>tabbed</w:t></w:r>\
             <w:hyperlink r:id=\"rLink\"><w:r><w:rPr><w:rStyle w:val=\"Hyperlink\"/></w:rPr><w:t>site</w:t></w:r></w:hyperlink>\
             <w:r><w:rPr><w:caps/><w:highlight w:val=\"yellow\"/></w:rPr><w:t>loud</w:t></w:r><w:r><w:rPr><w:vanish/></w:rPr><w:t>hidden</w:t></w:r>\
             <w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText> DATE </w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>May 1</w:t></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r>\
             <w:r><w:drawing><wp:anchor><wp:extent cx=\"952500\" cy=\"952500\"/><a:graphic><a:graphicData><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>In a box</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r>\
             <m:oMath><m:r><m:t>x=1</m:t></m:r></m:oMath></w:p>\
             {}{}{}{}{}\
             <w:tbl><w:tblPr><w:tblStyle w:val=\"TableGrid\"/></w:tblPr><w:tblGrid><w:gridCol w:w=\"1500\"/><w:gridCol w:w=\"3000\"/></w:tblGrid>\
             <w:tr><w:tc><w:tcPr><w:vMerge w:val=\"restart\"/></w:tcPr><w:p><w:r><w:t>Tall</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B</w:t></w:r></w:p></w:tc></w:tr>\
             <w:tr><w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p/></w:tc><w:tc><w:p><w:r><w:t>C</w:t></w:r></w:p></w:tc></w:tr></w:tbl>\
             <w:p><w:r><w:t>After</w:t></w:r><w:r><w:br w:type=\"page\"/></w:r></w:p>\
             <w:sectPr><w:footerReference w:type=\"default\" r:id=\"rfooter11\"/><w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/><w:cols w:space=\"720\"/></w:sectPr>\
             </w:body></w:document>",
            item(1, 0, "One"),
            item(1, 1, "One a"),
            item(2, 0, "Two"),
            item(3, 0, "Dot"),
            item(1, 0, "Three"),
        );
        let file = word_file(
            &document,
            &[
                ("word/styles.xml", &styles),
                ("word/theme/theme1.xml", theme),
                ("word/numbering.xml", &numbering),
                ("word/footer1.xml", &footer),
            ],
        );
        let mut report = Report::default();
        let read = read(&file, &mut report).unwrap();
        let blocks = model::normalize(read.blocks);
        let lost: Vec<Lost> = report.lost.iter().copied().collect();
        assert_eq!(
            lost,
            [
                Lost::TrackedChanges,
                Lost::TextBoxes,
                Lost::Equations,
                Lost::Fields,
                Lost::Tabs
            ]
        );
        let fonts: Vec<(&str, &str)> = report
            .fonts
            .iter()
            .map(|font| (font.from.as_str(), font.to.as_str()))
            .collect();
        assert_eq!(
            fonts,
            [("Calibri", "Carlito"), ("Calibri Light", "Carlito")]
        );
        let carlito = Some("Carlito".to_owned());
        assert!(
            matches!(&blocks[0], Block::Paragraph(_, inlines) if plain(inlines) == "Contents"),
            "{:?}",
            blocks[0]
        );
        assert_eq!(blocks[1], Block::Contents(ContentsStyle::Dotted));
        // A numbered heading keeps its number, and a style based on a
        // heading is one.
        let Block::Heading(1, para, inlines) = &blocks[2] else {
            panic!("{:?}", blocks[2]);
        };
        assert_eq!(plain(inlines), "1. Intro");
        assert_eq!((para.before, para.after), (Some(240), Some(0)));
        let Inline::Text(_, marks) = &inlines[1] else {
            panic!()
        };
        assert_eq!(marks.color, Color::from_hex("#2f5496"));
        assert_eq!(marks.size, Some(32));
        assert_eq!(marks.font, carlito);
        assert!(
            matches!(&blocks[3], Block::Heading(1, _, inlines) if matches!(&inlines[0], Inline::Text(text, marks) if text == "Plans" && marks.bold))
        );
        let Block::Paragraph(para, inlines) = &blocks[4] else {
            panic!("{:?}", blocks[4]);
        };
        assert_eq!((para.line, para.after), (Some(107), Some(160)));
        assert_eq!(plain(inlines), "Kept added tabbedsiteLOUDMay 1x=1");
        assert!(inlines.iter().any(|inline| matches!(inline, Inline::Text(text, marks) if text == "site" && marks.link.as_deref() == Some("https://example.com/a?b=1&c=2") && !marks.underline && marks.color.is_none())));
        assert!(inlines.iter().any(|inline| matches!(inline, Inline::Text(text, marks) if text == "LOUD" && marks.highlight == Color::from_hex("#ffff00"))));
        // The text box's text follows its paragraph.
        assert!(matches!(&blocks[5], Block::Paragraph(_, inlines) if plain(inlines) == "In a box"));
        // Lists sharing a definition carry on counting, as in Word, after
        // the heading that took the first number.
        let Block::Numbers(2, None, items) = &blocks[6] else {
            panic!("{:?}", blocks[6]);
        };
        assert_eq!(items.len(), 2);
        assert!(matches!(
            &items[0][1],
            Block::Numbers(1, Some(ListStyle::LowerAlpha), _)
        ));
        assert!(matches!(&blocks[7], Block::Bullets(None, items) if items.len() == 1));
        assert!(matches!(&blocks[8], Block::Numbers(4, None, items) if items.len() == 1));
        let Block::Table(rows) = &blocks[9] else {
            panic!("{:?}", blocks[9]);
        };
        assert_eq!(rows[0].cells[0].rowspan, 2);
        assert_eq!(rows[0].cells[0].colwidth, Some(vec![100]));
        assert_eq!(rows[1].cells.len(), 1);
        assert_eq!(rows[0].cells[1].borders, [Some(10); 4]);
        assert!(
            matches!(&rows[0].cells[1].content[0], Block::Paragraph(para, _) if para.after.is_none())
        );
        assert!(matches!(&blocks[10], Block::Paragraph(_, inlines) if plain(inlines) == "After"));
        assert_eq!(blocks[11], Block::PageBreak);
        assert_eq!(blocks.len(), 12);
        let footer = &read.parts.footer;
        assert!(
            matches!(&footer[0], Block::Paragraph(para, inlines) if para.align == Align::Center && inlines.len() == 2 && matches!(&inlines[1], Inline::Text(text, marks) if text == "#" && marks.field == Some(Field::PageNumber))),
            "{footer:?}"
        );
        assert_eq!(read.setup.unwrap().size, PageSize::Letter);
        assert_eq!(report.paper, None);
    }
}
