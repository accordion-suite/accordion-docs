//! What the Word and OpenDocument writers share: XML text, tables laid out
//! on a grid, and packing a document's parts into a zip file.
//!
//! Both formats are written straight from Docs' schema, so a download needs
//! no LibreOffice on the server and reads nothing a person sent in.

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::model::{Cell, MAX_SPAN, PageSetup, PageSize};

/// Escapes `text` for XML, leaving out characters XML 1.0 can't hold, such
/// as control characters, which would make Word refuse the whole file.
pub(crate) fn escape(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 || c == '\u{fffe}' || c == '\u{ffff}' => {}
            c => out.push(c),
        }
    }
}

pub(crate) fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape(&mut out, text);
    out
}

/// A length with up to two decimals and no trailing zeros, such as "612"
/// or "841.89".
pub(crate) fn number(value: f64) -> String {
    let text = format!("{value:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// The page to write: the document's own, or Letter with 1-inch margins
/// when it has none yet.
pub(crate) fn page_of(setup: Option<&PageSetup>) -> PageSetup {
    setup
        .copied()
        .unwrap_or_else(|| PageSetup::new(PageSize::Letter))
}

/// The room for text across the page, in points.
pub(crate) fn text_width(page: &PageSetup) -> f64 {
    let (width, _) = page.points();
    width - page.margins[1] - page.margins[3]
}

/// How large a picture is shown, in points: as wide as its width says, in
/// pixels at 96 to the inch, or as wide as it is, but never wider than
/// `room`.
pub(crate) fn picture_size(
    picture: &crate::picture::Picture,
    width: Option<u32>,
    room: f64,
) -> (f64, f64) {
    let wide = (f64::from(width.unwrap_or(picture.width)) * 0.75).min(room.max(36.0));
    let tall = wide * f64::from(picture.height) / f64::from(picture.width);
    (wide, tall)
}

/// A place in a table's grid.
pub(crate) enum Slot<'a> {
    /// A cell starts here, `span` columns wide and `rows` rows tall.
    Cell {
        cell: &'a Cell,
        span: usize,
        rows: usize,
    },
    /// The rest of a cell from a row above, `span` columns wide.
    Below { span: usize },
    /// Nothing: the row is shorter than the table.
    Empty,
}

/// A table laid out on a grid, as both formats want it: merged cells say
/// how far they reach, and the places they cover are marked.
pub(crate) struct Grid<'a> {
    pub columns: usize,
    pub rows: Vec<Vec<Slot<'a>>>,
    /// Each column's width in points, filling the room for text.
    pub widths: Vec<f64>,
}

impl<'a> Grid<'a> {
    pub fn new(rows: &'a [Vec<Cell>], room: f64) -> Self {
        // For each column, the last row a cell from above reaches down to,
        // and how wide that cell is, kept at its first column.
        let mut reach: Vec<Option<(usize, usize)>> = Vec::new();
        let mut laid = Vec::with_capacity(rows.len());
        let mut known: Vec<Option<f64>> = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            let mut slots = Vec::new();
            let mut column = 0;
            let mut cells = row.iter();
            loop {
                if let Some(Some((last, span))) = reach.get(column).copied()
                    && last >= index
                {
                    slots.push(Slot::Below { span });
                    column += span;
                    continue;
                }
                let Some(cell) = cells.next() else { break };
                let span = cell.colspan.clamp(1, MAX_SPAN) as usize;
                let tall = (cell.rowspan.clamp(1, MAX_SPAN) as usize).min(rows.len() - index);
                if reach.len() < column + span {
                    reach.resize(column + span, None);
                }
                if tall > 1 {
                    reach[column] = Some((index + tall - 1, span));
                    for covered in &mut reach[column + 1..column + span] {
                        *covered = None;
                    }
                } else {
                    reach[column] = None;
                }
                if let Some(widths) = &cell.colwidth {
                    if known.len() < column + span {
                        known.resize(column + span, None);
                    }
                    for (offset, width) in widths.iter().take(span).enumerate() {
                        if *width > 0 {
                            // Pixels as the editor draws them, at 96 to the
                            // inch, in points.
                            known[column + offset].get_or_insert(f64::from(*width) * 0.75);
                        }
                    }
                }
                slots.push(Slot::Cell {
                    cell,
                    span,
                    rows: tall,
                });
                column += span;
            }
            laid.push((slots, column));
        }
        let columns = laid
            .iter()
            .map(|(_, width)| *width)
            .max()
            .unwrap_or(0)
            .max(1);
        let rows = laid
            .into_iter()
            .map(|(mut slots, width)| {
                slots.extend((width..columns).map(|_| Slot::Empty));
                slots
            })
            .collect();
        known.resize(columns, None);
        Self {
            columns,
            rows,
            widths: widths(&known, room),
        }
    }

    /// The width of `span` columns from `column`, in points.
    pub fn width(&self, column: usize, span: usize) -> f64 {
        self.widths[column..(column + span).min(self.columns)]
            .iter()
            .sum()
    }
}

/// Column widths: the ones someone set, the rest sharing what's left, all
/// shrunk to fit if they're wider than the room for text.
fn widths(known: &[Option<f64>], room: f64) -> Vec<f64> {
    let set: f64 = known.iter().flatten().sum();
    let unset = known.iter().filter(|width| width.is_none()).count();
    let share = if unset == 0 {
        0.0
    } else {
        ((room - set) / unset as f64).max(36.0)
    };
    let mut widths: Vec<f64> = known.iter().map(|width| width.unwrap_or(share)).collect();
    let total: f64 = widths.iter().sum();
    if total > room && total > 0.0 {
        for width in &mut widths {
            *width *= room / total;
        }
    }
    widths
}

/// One part of a zip package.
pub(crate) struct Part<'a> {
    pub path: &'a str,
    pub contents: &'a [u8],
    /// Stored as is rather than compressed, as OpenDocument wants for its
    /// `mimetype`.
    pub stored: bool,
}

/// The parts as a zip file, in the order given.
pub(crate) fn pack(parts: &[Part]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for part in parts {
        let method = if part.stored {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        let options = SimpleFileOptions::default().compression_method(method);
        zip.start_file(part.path, options)
            .and_then(|()| zip.write_all(part.contents).map_err(Into::into))
            .expect("writing to memory doesn't fail");
    }
    zip.finish()
        .expect("writing to memory doesn't fail")
        .into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Block;

    fn cell(colspan: u32, rowspan: u32, colwidth: Option<Vec<u32>>) -> Cell {
        Cell {
            header: false,
            colspan,
            rowspan,
            colwidth,
            content: vec![Block::Paragraph(crate::model::Para::default(), Vec::new())],
        }
    }

    #[test]
    fn merged_cells_cover_their_places() {
        // | a (2 tall) | b c (2 wide)  |
        // |            | d | e         |
        // | f | g |                    (short row)
        let rows = vec![
            vec![cell(1, 2, Some(vec![100])), cell(2, 1, None)],
            vec![cell(1, 1, None), cell(1, 1, None)],
            vec![cell(1, 1, None)],
        ];
        let grid = Grid::new(&rows, 400.0);
        assert_eq!(grid.columns, 3);
        let shape: Vec<Vec<String>> = grid
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|slot| match slot {
                        Slot::Cell { span, rows, .. } => format!("cell {span}x{rows}"),
                        Slot::Below { span } => format!("below {span}"),
                        Slot::Empty => "empty".to_owned(),
                    })
                    .collect()
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                vec!["cell 1x2", "cell 2x1"],
                vec!["below 1", "cell 1x1", "cell 1x1"],
                vec!["cell 1x1", "empty", "empty"],
            ]
        );
        // The set column keeps its 75 points, and the others share the rest.
        assert_eq!(grid.widths, vec![75.0, 162.5, 162.5]);
    }

    #[test]
    fn text_xml_cant_hold_is_left_out() {
        assert_eq!(
            escaped("a\u{1}b <&> \"c\"\t"),
            "ab &lt;&amp;&gt; &quot;c&quot;\t"
        );
    }
}
