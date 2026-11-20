//! A document as Markdown, for **Download as** (decision 30).
//!
//! It writes CommonMark with GitHub's extensions: tables, checklists, and
//! strikethrough. What Markdown can't say is written as HTML, which
//! Markdown readers show: underlined text, superscript and subscript, and
//! tables with merged cells or with lists inside. Pictures are in the file
//! as `data:` addresses, so it stands alone. Centered, right-aligned, and
//! justified paragraphs are HTML with GitHub's `align`, and page breaks are
//! the `<div>` that Markdown-to-PDF tools break pages at. The page setup,
//! colors, sizes, fonts, indents, and spacing have no place in Markdown and
//! are left out, since GitHub drops the styles that would carry them.

use crate::html;
use crate::model::{Align, Block, Cell, Image, Inline, Marks, Para, Script};
use crate::picture::Pictures;

/// `blocks` as a Markdown file, with `pictures` in it.
pub fn write(blocks: &[Block], pictures: &Pictures) -> String {
    let mut writer = Writer { pictures };
    let mut out = String::new();
    writer.blocks(&mut out, blocks);
    let trimmed = out.trim_end_matches('\n').len();
    out.truncate(trimmed);
    out.push('\n');
    out
}

struct Writer<'a> {
    pictures: &'a Pictures,
}

impl Writer<'_> {
    /// Writes blocks one after another, with a blank line between them.
    fn blocks(&mut self, out: &mut String, blocks: &[Block]) {
        let mut last_list = None;
        for block in blocks {
            if is_blank(block) {
                continue;
            }
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push_str(if out.ends_with('\n') { "\n" } else { "\n\n" });
            }
            // Two lists in a row would read as one, so the second uses the
            // other marker.
            let other = last_list.is_some_and(|other: bool| !other);
            last_list = match block {
                Block::Bullets(..) | Block::Numbers(..) | Block::Checks(_) => Some(other),
                _ => None,
            };
            self.block(out, block, other);
        }
    }

    fn block(&mut self, out: &mut String, block: &Block, other_marker: bool) {
        match block {
            Block::Paragraph(para, inlines) if para.align != Align::Left => {
                aligned(out, "p", para, inlines);
            }
            Block::Heading(level, para, inlines) if para.align != Align::Left => {
                aligned(out, &format!("h{level}"), para, inlines);
            }
            Block::PageBreak => out.push_str("<div style=\"page-break-after: always;\"></div>"),
            Block::Paragraph(_, inlines) => write_inlines(out, inlines, false),
            Block::Heading(level, _, inlines) => {
                for _ in 0..*level {
                    out.push('#');
                }
                out.push(' ');
                write_inlines(out, inlines, true);
            }
            Block::Quote(blocks) => {
                let mut inner = String::new();
                self.blocks(&mut inner, blocks);
                prefix_lines(out, inner.trim_end_matches('\n'), "> ", "> ");
            }
            Block::Code(text) => {
                let fence = "`".repeat(longest_run(text, '`').max(2) + 1);
                out.push_str(&fence);
                out.push('\n');
                out.push_str(text);
                if !text.is_empty() && !text.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(&fence);
            }
            Block::Rule => out.push_str("---"),
            Block::Image(image) => self.image(out, image),
            Block::Bullets(_, items) => {
                let marker = if other_marker { "* " } else { "- " };
                self.items(out, items.iter().map(|item| (marker.to_owned(), item)));
            }
            Block::Numbers(start, _, items) => {
                let dot = if other_marker { ')' } else { '.' };
                self.items(
                    out,
                    items.iter().enumerate().map(|(index, item)| {
                        (format!("{}{dot} ", u64::from(*start) + index as u64), item)
                    }),
                );
            }
            Block::Checks(items) => {
                let bullet = if other_marker { '*' } else { '-' };
                self.items(
                    out,
                    items.iter().map(|(checked, item)| {
                        let mark = if *checked { 'x' } else { ' ' };
                        (format!("{bullet} [{mark}] "), item)
                    }),
                );
            }
            Block::Table(rows) => {
                if is_plain_table(rows) {
                    write_table(out, rows);
                } else {
                    out.push_str(&without_blank_lines(
                        html::fragment(std::slice::from_ref(block), self.pictures).trim_end(),
                    ));
                }
            }
        }
    }

    /// Writes list items: the marker before each item's first line, and its
    /// other lines lined up under the item's text.
    fn items<'b>(
        &mut self,
        out: &mut String,
        items: impl Iterator<Item = (String, &'b Vec<Block>)>,
    ) {
        for (index, (marker, blocks)) in items.enumerate() {
            if index > 0 {
                out.push('\n');
            }
            let mut inner = String::new();
            for (index, block) in blocks.iter().enumerate() {
                if index > 0 {
                    // A list right under the item's text keeps the list tight.
                    // A numbered list that starts past 1 can't follow text
                    // without a blank line, or it reads as more of the text.
                    let nested = matches!(
                        block,
                        Block::Bullets(..) | Block::Numbers(1, ..) | Block::Checks(_)
                    );
                    inner.push_str(if nested { "\n" } else { "\n\n" });
                }
                self.block(&mut inner, block, false);
            }
            // A checklist item's box is part of its text, so what follows lines
            // up under the box, not after it.
            let width = marker
                .strip_suffix("[ ] ")
                .or(marker.strip_suffix("[x] "))
                .unwrap_or(&marker)
                .len();
            prefix_lines(out, &inner, &marker, &" ".repeat(width));
        }
    }

    /// A picture, with its contents in its address. One whose contents are
    /// missing keeps only its words.
    fn image(&mut self, out: &mut String, image: &Image) {
        out.push_str("![");
        let mut line_start = false;
        push_text(out, &image.alt, &mut line_start);
        out.push_str("](");
        if let Some(picture) = self.pictures.get(&image.hash) {
            out.push_str(&picture.data_address());
        }
        out.push(')');
    }
}

/// A paragraph with nothing in it, which Markdown has no way to keep.
/// A centered, right-aligned, or justified paragraph or heading, as HTML
/// with the `align` that GitHub keeps.
fn aligned(out: &mut String, tag: &str, para: &Para, inlines: &[Inline]) {
    out.push_str(&format!("<{tag} align=\"{}\">", para.align.name()));
    out.push_str(&html::inline_fragment(inlines));
    out.push_str(&format!("</{tag}>"));
}

fn is_blank(block: &Block) -> bool {
    matches!(block, Block::Paragraph(_, inlines) if inlines.iter().all(|inline| matches!(inline, Inline::Text(text, _) if text.is_empty())))
}

/// Writes `text` with `first` before its first line and `rest` before each
/// other line. Blank lines get the prefix without its trailing spaces.
fn prefix_lines(out: &mut String, text: &str, first: &str, rest: &str) {
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let prefix = if index == 0 { first } else { rest };
        if line.is_empty() {
            out.push_str(prefix.trim_end());
        } else {
            out.push_str(prefix);
            out.push_str(line);
        }
    }
}

/// Whether a table fits Markdown's: no merged cells, and only text in each
/// cell.
fn is_plain_table(rows: &[Vec<Cell>]) -> bool {
    let columns = rows.first().map_or(0, Vec::len);
    columns > 0
        && rows.iter().all(|row| {
            row.len() == columns
                && row.iter().all(|cell| {
                    cell.colspan == 1
                        && cell.rowspan == 1
                        && cell
                            .content
                            .iter()
                            .all(|block| matches!(block, Block::Paragraph(..) | Block::Heading(..)))
                })
        })
}

/// A table as Markdown's: the first row is its heading, and each cell's
/// paragraphs are joined by line breaks.
fn write_table(out: &mut String, rows: &[Vec<Cell>]) {
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push('|');
        for cell in row {
            let mut text = String::new();
            for (index, block) in cell.content.iter().enumerate() {
                if index > 0 {
                    text.push_str("<br>");
                }
                if let Block::Paragraph(_, inlines) | Block::Heading(_, _, inlines) = block {
                    write_inlines(&mut text, inlines, true);
                }
            }
            out.push(' ');
            out.push_str(&text);
            out.push_str(" |");
        }
        if index == 0 {
            out.push_str("\n|");
            for _ in row {
                out.push_str(" --- |");
            }
        }
    }
}

/// HTML with no blank lines in it, since a blank line ends HTML in
/// Markdown. A blank line's line break is written as `&#10;` instead, which
/// reads the same in HTML, such as in code inside a table.
fn without_blank_lines(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    for (index, line) in html.split('\n').enumerate() {
        if index > 0 {
            out.push_str(if line.trim().is_empty() {
                "&#10;"
            } else {
                "\n"
            });
        }
        out.push_str(line);
    }
    out
}

/// The longest run of `c` in `text`.
fn longest_run(text: &str, c: char) -> usize {
    let (mut longest, mut run) = (0, 0);
    for found in text.chars() {
        run = if found == c { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

/// The marks Markdown writes around text, outermost first.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Link,
    Strike,
    Bold,
    Italic,
    Underline,
    Super,
    Sub,
}

fn marks_of(marks: &Marks) -> Vec<Mark> {
    let mut list = Vec::new();
    for (on, mark) in [
        (marks.link.is_some(), Mark::Link),
        (marks.strike, Mark::Strike),
        (marks.bold, Mark::Bold),
        (marks.italic, Mark::Italic),
        (marks.underline, Mark::Underline),
        (marks.script == Some(Script::Super), Mark::Super),
        (marks.script == Some(Script::Sub), Mark::Sub),
    ] {
        if on {
            list.push(mark);
        }
    }
    list
}

fn opener(mark: Mark) -> &'static str {
    match mark {
        Mark::Link => "[",
        Mark::Strike => "~~",
        Mark::Bold => "**",
        Mark::Italic => "*",
        Mark::Underline => "<u>",
        Mark::Super => "<sup>",
        Mark::Sub => "<sub>",
    }
}

fn closer(mark: Mark, link: Option<&str>) -> String {
    match mark {
        Mark::Link => format!("]({})", link_target(link.unwrap_or_default())),
        Mark::Strike => "~~".to_owned(),
        Mark::Bold => "**".to_owned(),
        Mark::Italic => "*".to_owned(),
        Mark::Underline => "</u>".to_owned(),
        Mark::Super => "</sup>".to_owned(),
        Mark::Sub => "</sub>".to_owned(),
    }
}

/// A link's address as Markdown writes it: in angle brackets when it has
/// spaces or brackets in it.
fn link_target(href: &str) -> String {
    if href.contains([' ', '(', ')', '<', '>']) {
        format!("<{}>", href.replace('<', "%3C").replace('>', "%3E"))
    } else {
        href.to_owned()
    }
}

/// Writes runs of text with their marks. Marks open and close only where
/// they change, so a bold run with an italic word in it stays one bold run.
/// Spaces at a run's edges go outside its marks, since Markdown doesn't
/// read `** bold**` as bold. `one_line` turns line breaks into spaces, for
/// headings and table cells.
fn write_inlines(out: &mut String, inlines: &[Inline], one_line: bool) {
    let mut open: Vec<Mark> = Vec::new();
    let mut link: Option<String> = None;
    // Spaces held back until it's known whether marks close before them.
    let mut pending = String::new();
    let mut line_start = true;
    for inline in inlines {
        let (text, marks) = match inline {
            Inline::Text(text, marks) if !text.is_empty() => (text.as_str(), marks.clone()),
            Inline::Text(..) => continue,
            Inline::Break => {
                close_marks(out, &mut open, link.as_deref(), 0);
                out.push_str(&pending);
                pending.clear();
                if one_line {
                    out.push(' ');
                } else {
                    out.push_str("\\\n");
                    line_start = true;
                }
                continue;
            }
        };
        let wanted = marks_of(&marks);
        // Keep the open marks that this run also has, from the outside in,
        // as long as the link is the same one.
        let mut keep = 0;
        while keep < open.len()
            && wanted.get(keep) == Some(&open[keep])
            && (open[keep] != Mark::Link || link == marks.link)
        {
            keep += 1;
        }
        let trimmed_start = text.trim_start();
        let leading = &text[..text.len() - trimmed_start.len()];
        let body = trimmed_start.trim_end();
        let trailing = &trimmed_start[body.len()..];
        if keep < open.len() {
            close_marks(out, &mut open, link.as_deref(), keep);
        }
        out.push_str(&pending);
        pending.clear();
        if body.is_empty() {
            // Only spaces: they need no marks of their own.
            push_text(out, leading, &mut line_start);
            pending.push_str(trailing);
            continue;
        }
        push_text(out, leading, &mut line_start);
        for &mark in &wanted[keep..] {
            if mark == Mark::Link {
                link = marks.link.clone();
            }
            out.push_str(opener(mark));
            open.push(mark);
            line_start = false;
        }
        if marks.code {
            let fence = "`".repeat(longest_run(body, '`') + 1);
            let pad = if body.starts_with('`') || body.ends_with('`') {
                " "
            } else {
                ""
            };
            out.push_str(&fence);
            out.push_str(pad);
            out.push_str(body);
            out.push_str(pad);
            out.push_str(&fence);
            line_start = false;
        } else {
            push_text(out, body, &mut line_start);
        }
        pending.push_str(trailing);
    }
    close_marks(out, &mut open, link.as_deref(), 0);
    out.push_str(&pending);
}

/// Closes the open marks down to the first `keep`.
fn close_marks(out: &mut String, open: &mut Vec<Mark>, link: Option<&str>, keep: usize) {
    while open.len() > keep {
        let mark = open.pop().expect("there's a mark to close");
        out.push_str(&closer(mark, link));
    }
}

/// Writes plain text, with the characters Markdown would read as markup
/// escaped.
fn push_text(out: &mut String, text: &str, line_start: &mut bool) {
    for c in text.chars() {
        if *line_start {
            if c == ' ' || c == '\t' {
                // Spaces at the start of a line would be dropped, or make
                // the line code.
                out.push_str("&#32;");
                *line_start = false;
                continue;
            }
            if matches!(c, '#' | '-' | '+' | '=') {
                out.push('\\');
            }
        }
        match c {
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '~' | '|' | '&' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push(' '),
            _ => out.push(c),
        }
        *line_start = false;
    }
    // A number at the start of a line followed by a dot or parenthesis
    // reads as a numbered list, so those are escaped as they're written.
    escape_list_number(out);
}

/// Escapes the dot or parenthesis in "1. " or "1) " at the start of the
/// last line written, so it doesn't read as a numbered list.
fn escape_list_number(out: &mut String) {
    let line = out.rsplit('\n').next().unwrap_or_default();
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 9 {
        return;
    }
    let rest = &line[digits..];
    if (rest.starts_with(". ") || rest.starts_with(") ") || rest == "." || rest == ")")
        && !rest.starts_with("\\")
    {
        let at = out.len() - line.len() + digits;
        out.insert(at, '\\');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> Inline {
        Inline::Text(text.to_owned(), Marks::default())
    }

    fn marked(text: &str, marks: Marks) -> Inline {
        Inline::Text(text.to_owned(), marks)
    }

    fn paragraph(inlines: Vec<Inline>) -> Block {
        Block::Paragraph(Para::default(), inlines)
    }

    #[test]
    fn writes_each_kind_of_block() {
        let bold = Marks {
            bold: true,
            ..Marks::default()
        };
        let both = Marks {
            bold: true,
            italic: true,
            ..Marks::default()
        };
        let blocks = vec![
            Block::Heading(2, Para::default(), vec![text("Plan")]),
            paragraph(vec![
                text("Some "),
                marked("bold ", bold.clone()),
                marked("and italic", both),
                marked(" text", bold),
                text(" here."),
            ]),
            Block::Bullets(
                None,
                vec![
                    vec![paragraph(vec![text("One")])],
                    vec![
                        paragraph(vec![text("Two")]),
                        Block::Numbers(1, None, vec![vec![paragraph(vec![text("Three")])]]),
                    ],
                    vec![
                        paragraph(vec![text("Four")]),
                        Block::Numbers(5, None, vec![vec![paragraph(vec![text("Five")])]]),
                    ],
                ],
            ),
            Block::Checks(vec![
                (true, vec![paragraph(vec![text("Done")])]),
                (
                    false,
                    vec![
                        paragraph(vec![text("Not yet")]),
                        Block::Checks(vec![(false, vec![paragraph(vec![text("Soon")])])]),
                    ],
                ),
            ]),
            Block::Quote(vec![paragraph(vec![
                text("Said"),
                Inline::Break,
                text("twice"),
            ])]),
            Block::Code("let a = `b`;\n".to_owned()),
            Block::Rule,
        ];
        assert_eq!(
            write(&blocks, &Pictures::new()),
            "## Plan\n\n\
             Some **bold *and italic* text** here.\n\n\
             - One\n\
             - Two\n  1. Three\n\
             - Four\n\n  5. Five\n\n\
             * [x] Done\n\
             * [ ] Not yet\n  - [ ] Soon\n\n\
             > Said\\\n> twice\n\n\
             ```\nlet a = `b`;\n```\n\n\
             ---\n"
        );
    }

    #[test]
    fn markup_in_text_stays_text() {
        let blocks = vec![
            paragraph(vec![text("# not a heading, *not* [a link](x) & <b>")]),
            paragraph(vec![text("1. not a list")]),
            paragraph(vec![text("  indented")]),
        ];
        assert_eq!(
            write(&blocks, &Pictures::new()),
            "\\# not a heading, \\*not\\* \\[a link\\](x) \\& \\<b\\>\n\n\
             1\\. not a list\n\n\
             &#32; indented\n"
        );
    }

    #[test]
    fn links_and_code_keep_their_text() {
        let link = Marks {
            link: Some("https://example.com/a b".to_owned()),
            ..Marks::default()
        };
        let code = Marks {
            code: true,
            ..Marks::default()
        };
        let blocks = vec![paragraph(vec![
            marked("the site", link),
            text(" and "),
            marked("a `tick`", code),
        ])];
        assert_eq!(
            write(&blocks, &Pictures::new()),
            "[the site](<https://example.com/a b>) and `` a `tick` ``\n"
        );
    }

    #[test]
    fn tables_with_merged_cells_are_html() {
        let cell = |text_: &str, colspan| Cell {
            header: false,
            colspan,
            rowspan: 1,
            colwidth: None,
            content: vec![paragraph(vec![text(text_)])],
        };
        let plain = Block::Table(vec![
            vec![cell("a", 1), cell("b|c", 1)],
            vec![cell("d", 1), cell("e", 1)],
        ]);
        assert_eq!(
            write(&[plain], &Pictures::new()),
            "| a | b\\|c |\n| --- | --- |\n| d | e |\n"
        );
        let merged = Block::Table(vec![
            vec![cell("wide", 2)],
            vec![cell("d", 1), cell("e", 1)],
        ]);
        let written = write(&[merged], &Pictures::new());
        assert!(written.starts_with("<table>"), "{written}");
        assert!(written.contains("colspan=\"2\""), "{written}");
    }

    #[test]
    fn aligned_paragraphs_and_page_breaks_are_html() {
        let markdown = write(&crate::html::tests::laid_out(), &Pictures::new());
        assert!(markdown.starts_with(
            "<p align=\"center\">Trip</p>\n\nSummer\n\n<h5 align=\"right\">Days</h5>"
        ));
        assert!(markdown.contains("<div style=\"page-break-after: always;\"></div>"));
    }
}
