//! **Open with Docs** (decision 30): a new document from a Word or
//! OpenDocument file, a web page, Markdown, or plain text. Older Word files
//! and Rich Text are first made Word files by LibreOffice, in the sandbox
//! too, where it's installed.
//!
//! The file is untrusted, so it's read in the sandbox (decision 1), by the
//! server's own program answering [`COMMAND`], as it answers core's
//! `office-text`. The command writes a line of JSON, the [`Report`] of what
//! didn't come across, then the new document's page, which the server
//! reads again through the schema like any page, so nothing the command
//! writes reaches a document unchecked.

use std::collections::BTreeSet;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;

use serde::{Deserialize, Serialize};

use crate::comments::{self, Comment, Mark, Thread};
use crate::fonts::Replaced;
use crate::html;
use crate::model::{self, Block, Inline, Marks, PageSetup, PageSize, Para, Parts};
use crate::picture::Pictures;

mod flat;
mod odt;
mod word;
mod xml;

/// The command the server's program answers, followed by the file's
/// extension, with the file on standard input.
pub const COMMAND: &str = "docs-import";

/// The largest file Docs opens. Pictures in it are shrunk afterwards.
pub const MAX_FILE: u64 = 100 * 1024 * 1024;

/// The formats Docs opens, by their file name extensions.
pub const EXTENSIONS: &[&str] = &[
    "docx", "docm", "dotx", "dotm", "odt", "ott", "html", "htm", "xhtml", "md", "markdown", "txt",
];

/// The formats LibreOffice makes Word files of first: Word 97–2003 files
/// and Rich Text, which Docs doesn't read itself.
pub const THROUGH_LIBREOFFICE: &[&str] = &["doc", "rtf"];

/// What didn't come across from a file, for the warning before it's
/// converted.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub lost: BTreeSet<Lost>,
    /// Fonts Docs doesn't have, and the ones it uses in their place.
    pub fonts: Vec<Replaced>,
    /// The paper Docs uses, when the file's isn't one Docs has.
    pub paper: Option<PageSize>,
}

impl Report {
    /// Whether anything would be lost or changed, which people are asked
    /// about first. Fonts alone aren't: the editor says which it used.
    pub fn warns(&self) -> bool {
        !self.lost.is_empty() || self.paper.is_some()
    }
}

/// Something in a file that Docs leaves out or makes plainer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lost {
    /// Word's tracked changes, which are taken as accepted.
    TrackedChanges,
    /// Text boxes, whose text becomes ordinary paragraphs.
    TextBoxes,
    /// Shapes, charts, SmartArt, and embedded objects.
    Shapes,
    /// Equations, which become plain text.
    Equations,
    /// Pictures in formats Docs doesn't show, such as EMF or TIFF, or ones
    /// elsewhere on the web.
    Pictures,
    /// Pictures in a line of text, which go on lines of their own.
    PicturesMoved,
    /// Fields other than page numbers and tables of contents, such as
    /// dates, which keep the text they last showed.
    Fields,
    /// Endnotes, which become footnotes.
    Endnotes,
    /// Text in columns.
    Columns,
    /// Sections with paper or margins of their own.
    Sections,
    /// Headers and footers of their own on even pages.
    EvenPages,
    /// Tabs, which become spaces.
    Tabs,
}

/// A document read from a file, before it's written as Docs' page.
#[derive(Debug, Default)]
pub struct Converted {
    pub blocks: Vec<Block>,
    pub parts: Parts,
    pub setup: Option<PageSetup>,
    pub pictures: Pictures,
    /// Comment threads, whose text the blocks mark with
    /// [`Inline::CommentStart`] and [`Inline::CommentEnd`]. Their
    /// positions are made once the document is.
    pub comments: Vec<Thread>,
}

/// A comment as a file has it.
pub(crate) struct FileComment {
    pub name: String,
    pub text: String,
    /// When it was written, as the file writes it.
    pub at: Option<String>,
}

/// A thread of `comments` read from a file, the first one and its replies,
/// by people who may have no account here, so only their names are kept.
pub(crate) fn file_thread(comments: Vec<FileComment>, resolved: bool) -> Thread {
    let comments: Vec<Comment> = comments
        .into_iter()
        .map(|comment| Comment {
            id: comments::new_id(),
            author: None,
            name: comment.name.trim().to_owned(),
            text: comment.text.trim().to_owned(),
            at: comment
                .at
                .as_deref()
                .and_then(file_time)
                .unwrap_or_else(comments::now),
            edited: None,
            mentions: Vec::new(),
        })
        .collect();
    let resolved = resolved.then(|| Mark {
        by: None,
        name: String::new(),
        at: comments
            .last()
            .map(|comment| comment.at.clone())
            .unwrap_or_else(comments::now),
    });
    Thread {
        id: comments::new_id(),
        start: String::new(),
        end: String::new(),
        quote: String::new(),
        resolved,
        comments,
    }
}

/// A time a file writes, in RFC 3339. A time without a zone is taken as
/// UTC, since there's no telling where it was written.
fn file_time(at: &str) -> Option<String> {
    let at = at.trim();
    let time = at.parse::<jiff::Timestamp>().ok().or_else(|| {
        at.parse::<jiff::civil::DateTime>()
            .ok()
            .and_then(|civil| jiff::tz::TimeZone::UTC.to_timestamp(civil).ok())
    })?;
    Some(time.round(jiff::Unit::Millisecond).ok()?.to_string())
}

/// Runs the command: reads the file on standard input, and writes the
/// report and the page to standard output. Returns the exit code.
pub fn main(mut args: impl Iterator<Item = std::ffi::OsString>) -> i32 {
    let Some(extension) = args.next().and_then(|arg| arg.into_string().ok()) else {
        eprintln!("Usage: {COMMAND} <extension>");
        return 2;
    };
    let mut contents = Vec::new();
    let read = io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .map(std::fs::File::from)
        .and_then(|file| file.take(MAX_FILE + 1).read_to_end(&mut contents));
    if let Err(err) = read {
        eprintln!("couldn't read the file: {err}");
        return 1;
    }
    if contents.len() as u64 > MAX_FILE {
        eprintln!("the file is larger than Docs opens");
        return 1;
    }
    let (converted, report) = match convert(&extension, &contents) {
        Ok(done) => done,
        Err(problem) => {
            eprintln!("{problem}");
            return 1;
        }
    };
    let page = html::page_with_comments(
        &converted.blocks,
        &converted.parts,
        converted.setup.as_ref(),
        None,
        &converted.pictures,
        &converted.comments,
    );
    let mut out = io::stdout().lock();
    let written = serde_json::to_writer(&mut out, &report)
        .map_err(io::Error::other)
        .and_then(|()| out.write_all(b"\n"))
        .and_then(|()| out.write_all(page.as_bytes()))
        .and_then(|()| out.flush());
    match written {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("couldn't write the document: {err}");
            1
        }
    }
}

/// The document in `contents`, a file with the extension `extension`, and
/// what didn't come across.
pub fn convert(extension: &str, contents: &[u8]) -> Result<(Converted, Report), String> {
    let mut report = Report::default();
    let mut converted = match extension.to_ascii_lowercase().as_str() {
        "docx" | "docm" | "dotx" | "dotm" => word::read(contents, &mut report)?,
        "odt" | "ott" => odt::read(contents, &mut report)?,
        "html" | "htm" | "xhtml" => {
            if pictures_from_elsewhere(contents) {
                report.lost.insert(Lost::Pictures);
            }
            page(contents)
        }
        "md" | "markdown" => markdown(&text_of(contents), &mut report),
        "txt" => plain(&text_of(contents)),
        other => return Err(format!("Docs doesn't open .{other} files")),
    };
    converted.blocks = model::normalize(converted.blocks);
    converted.parts = converted.parts.normalized();
    Ok((converted, report))
}

/// Whether a page shows pictures from elsewhere on the web, which Docs
/// leaves out, rather than ones inside it.
fn pictures_from_elsewhere(contents: &[u8]) -> bool {
    let text = String::from_utf8_lossy(contents);
    let page = scraper::Html::parse_document(&text);
    page.root_element().descendants().any(|node| {
        node.value().as_element().is_some_and(|element| {
            element.name() == "img"
                && element
                    .attr("src")
                    .is_some_and(|src| !src.trim_start().starts_with("data:"))
        })
    })
}

fn page(contents: &[u8]) -> Converted {
    let html::Document {
        blocks,
        setup,
        pictures,
        parts,
    } = html::read_document(contents);
    Converted {
        blocks,
        parts,
        setup,
        pictures,
        comments: Vec::new(),
    }
}

/// Text in UTF-8, or in UTF-16 with its byte order mark. Text that isn't
/// UTF-8 is read as Windows' Western European, the usual older encoding.
fn text_of(contents: &[u8]) -> String {
    if let Some(rest) = contents.strip_prefix(b"\xef\xbb\xbf") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    let utf16 = |rest: &[u8], little: bool| {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                if little {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = contents.strip_prefix(b"\xff\xfe") {
        return utf16(rest, true);
    }
    if let Some(rest) = contents.strip_prefix(b"\xfe\xff") {
        return utf16(rest, false);
    }
    match std::str::from_utf8(contents) {
        Ok(text) => text.to_owned(),
        Err(_) => contents.iter().map(|byte| windows_1252(*byte)).collect(),
    }
}

/// A byte of Windows-1252 as a character.
fn windows_1252(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
        '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
        'ž', 'Ÿ',
    ];
    match byte {
        0x80..=0x9f => HIGH[usize::from(byte - 0x80)],
        byte => char::from(byte),
    }
}

/// Plain text: a paragraph for each line.
fn plain(text: &str) -> Converted {
    let blocks = text
        .lines()
        .map(|line| {
            let line = line.replace('\t', "    ");
            let inlines = if line.is_empty() {
                Vec::new()
            } else {
                vec![Inline::Text(line, Marks::default())]
            };
            Block::Paragraph(Para::default(), inlines)
        })
        .collect();
    Converted {
        blocks,
        ..Converted::default()
    }
}

/// Markdown as GitHub reads it, with tables, task lists, strikethrough,
/// and footnotes, through HTML. Footnotes are written as GitHub writes
/// them, which Docs' page reader knows.
fn markdown(text: &str, report: &mut Report) -> Converted {
    use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd};

    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    // Each footnote's ID in the page, from its label.
    let id = |label: &str| {
        let safe: String = label
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect();
        format!("fn-{safe}")
    };
    let mut body: Vec<Event> = Vec::new();
    let mut notes: Vec<(String, Vec<Event>)> = Vec::new();
    let mut in_note: Option<(String, Vec<Event>)> = None;
    let mut numbers: Vec<String> = Vec::new();
    for event in Parser::new_ext(text, options) {
        let event = match event {
            Event::Start(Tag::FootnoteDefinition(label)) => {
                in_note = Some((id(&label), Vec::new()));
                continue;
            }
            Event::End(TagEnd::FootnoteDefinition) => {
                if let Some(note) = in_note.take() {
                    notes.push(note);
                }
                continue;
            }
            Event::FootnoteReference(label) => {
                let target = id(&label);
                if !numbers.contains(&target) {
                    numbers.push(target.clone());
                }
                Event::InlineHtml(CowStr::from(format!(
                    "<sup><a href=\"#{target}\" data-footnote-ref>*</a></sup>"
                )))
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                if !dest_url.starts_with("data:") {
                    report.lost.insert(Lost::Pictures);
                }
                Event::Start(Tag::Image {
                    link_type: pulldown_cmark::LinkType::Inline,
                    dest_url,
                    title: CowStr::Borrowed(""),
                    id: CowStr::Borrowed(""),
                })
            }
            event => event,
        };
        match &mut in_note {
            Some((_, events)) => events.push(event),
            None => body.push(event),
        }
    }
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, body.into_iter());
    if !notes.is_empty() {
        html.push_str("<section data-footnotes><ol>");
        for (target, events) in notes {
            html.push_str(&format!("<li id=\"{target}\">"));
            pulldown_cmark::html::push_html(&mut html, events.into_iter());
            html.push_str("</li>");
        }
        html.push_str("</ol></section>");
    }
    page(html.as_bytes())
}

/// A document with comments, and what reading it back must find, for the
/// Word and OpenDocument readers' tests.
#[cfg(test)]
pub(crate) mod comment_fixture {
    use super::*;

    fn comment(name: &str, text: &str, at: &str) -> Comment {
        Comment {
            id: comments::new_id(),
            author: None,
            name: name.into(),
            text: text.into(),
            at: at.into(),
            edited: None,
            mentions: Vec::new(),
        }
    }

    fn thread(id: &str, resolved: bool, comments: Vec<Comment>) -> Thread {
        Thread {
            id: id.into(),
            start: "AQ==".into(),
            end: "AQ==".into(),
            quote: String::new(),
            resolved: resolved.then(|| Mark {
                by: None,
                name: "Dana Lee".into(),
                at: "2026-11-25T12:00:00Z".into(),
            }),
            comments,
        }
    }

    /// Two paragraphs with a comment on a word of each, a reply to the
    /// first, the second resolved, and a third whose text is gone.
    pub(crate) fn document() -> (Vec<Block>, Vec<Thread>) {
        let text = |words: &str| Inline::Text(words.into(), Marks::default());
        let blocks = vec![
            Block::Paragraph(
                Para::default(),
                vec![
                    text("Hello "),
                    Inline::CommentStart("a".into()),
                    text("big"),
                    Inline::CommentEnd("a".into()),
                    text(" world"),
                ],
            ),
            Block::Paragraph(
                Para::default(),
                vec![
                    Inline::CommentStart("b".into()),
                    text("Second"),
                    Inline::CommentEnd("b".into()),
                    text(" line"),
                ],
            ),
        ];
        let threads = vec![
            thread(
                "a",
                false,
                vec![
                    comment("Dana Lee", "First\nsecond line", "2026-11-25T10:00:00.250Z"),
                    comment("Sam Kim", "Sure & <done>", "2026-11-25T11:00:00Z"),
                ],
            ),
            thread(
                "b",
                true,
                vec![comment("Dana Lee", "Fixed?", "2026-11-25T10:30:00Z")],
            ),
            thread(
                "gone",
                false,
                vec![comment(
                    "Sam Kim",
                    "Its text is gone",
                    "2026-11-25T09:00:00Z",
                )],
            ),
        ];
        (blocks, threads)
    }

    /// Checks that `read` has the fixture's threads, on the same text.
    pub(crate) fn check(read: &Converted) {
        assert_eq!(read.comments.len(), 3, "{:?}", read.comments);
        let quotes = model::comment_quotes(&read.blocks);
        let by_text = |text: &str| {
            read.comments
                .iter()
                .find(|thread| thread.comments[0].text == text)
                .unwrap_or_else(|| panic!("no thread starting {text:?}: {:?}", read.comments))
        };
        let a = by_text("First\nsecond line");
        assert_eq!(a.comments.len(), 2);
        assert_eq!(a.comments[0].name, "Dana Lee");
        assert_eq!(a.comments[0].at, "2026-11-25T10:00:00Z");
        assert_eq!(a.comments[1].name, "Sam Kim");
        assert_eq!(a.comments[1].text, "Sure & <done>");
        assert_eq!(a.comments[1].at, "2026-11-25T11:00:00Z");
        assert!(a.resolved.is_none());
        assert_eq!(quotes.get(&a.id).map(String::as_str), Some("big"));
        let b = by_text("Fixed?");
        assert!(b.resolved.is_some());
        assert_eq!(quotes.get(&b.id).map(String::as_str), Some("Second"));
        let gone = by_text("Its text is gone");
        assert_eq!(quotes.get(&gone.id).map(String::as_str), Some(""));
        let ranged = model::ranged_comments(&read.blocks);
        assert!(ranged.contains(&a.id) && ranged.contains(&b.id));
        // The text itself is as it was.
        let mut plain = read.blocks.clone();
        model::without_comment_marks(&mut plain);
        let (mut written, _) = document();
        model::without_comment_marks(&mut written);
        assert_eq!(model::normalize(plain), model::normalize(written));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_markdown_as_github_does() {
        let (converted, report) = convert(
            "md",
            b"# Plans\n\nSome **bold** and ~~gone~~ text.[^1]\n\n- [x] Done\n- [ ] Not yet\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n![cat](cat.png)\n\n[^1]: A note.\n",
        )
        .unwrap();
        let blocks = &converted.blocks;
        assert!(
            matches!(&blocks[0], Block::Heading(1, _, inlines) if inlines == &vec![Inline::Text("Plans".into(), Marks::default())])
        );
        let Block::Paragraph(_, inlines) = &blocks[1] else {
            panic!("{blocks:?}");
        };
        assert!(inlines.iter().any(
            |inline| matches!(inline, Inline::Text(text, marks) if text == "bold" && marks.bold)
        ));
        assert!(inlines.iter().any(
            |inline| matches!(inline, Inline::Text(text, marks) if text == "gone" && marks.strike)
        ));
        assert!(inlines.iter().any(|inline| matches!(inline, Inline::Footnote(note) if note == &vec![Inline::Text("A note.".into(), Marks::default())])));
        assert!(
            matches!(&blocks[2], Block::Checks(items) if items.len() == 2 && items[0].0 && !items[1].0)
        );
        assert!(matches!(&blocks[3], Block::Table(rows) if rows.len() == 2));
        assert!(report.lost.contains(&Lost::Pictures));
    }

    #[test]
    fn reads_plain_text_line_by_line() {
        let (converted, report) = convert("txt", b"\xef\xbb\xbfOne\n\nTwo\tthree\r\n").unwrap();
        assert_eq!(converted.blocks.len(), 3);
        assert!(
            matches!(&converted.blocks[2], Block::Paragraph(_, inlines) if inlines == &vec![Inline::Text("Two    three".into(), Marks::default())])
        );
        assert_eq!(report, Report::default());
        let (latin, _) = convert("txt", b"Caf\xe9 \x93quoted\x94").unwrap();
        assert!(
            matches!(&latin.blocks[0], Block::Paragraph(_, inlines) if inlines == &vec![Inline::Text("Café “quoted”".into(), Marks::default())])
        );
    }

    #[test]
    fn refuses_formats_it_doesnt_open() {
        assert!(convert("pdf", b"%PDF").is_err());
        assert!(convert("docx", b"not a zip").is_err());
    }
}
