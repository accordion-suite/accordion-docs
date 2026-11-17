//! A document as core's co-editing service sees it (decision 30): the file
//! is HTML with the co-editing state in its data block, and the document
//! is y-prosemirror's XML fragment.
//!
//! Core keeps the state between saves and merges a change made outside
//! Docs three ways, from the state at the last save. This format only has to
//! read and write files, and make a document match a file with few edits.

use accordion_core::coedit::{Format, FormatError};
use yrs::{Doc, ReadTxn, StateVector, Transact, XmlFragment};

use crate::html;
use crate::model::Block;
use crate::yjs;

pub(crate) struct DocFormat;

impl Format for DocFormat {
    fn name(&self) -> &'static str {
        "docs"
    }

    fn state_in(&self, contents: &[u8]) -> Option<Vec<u8>> {
        html::state_in(contents)
    }

    /// Replaces only the blocks that differ, so people typing elsewhere in
    /// the document keep their place, and their typing.
    fn absorb(&self, doc: &Doc, contents: &[u8]) -> Result<(), FormatError> {
        let wanted = html::read(contents);
        let fragment = doc.get_or_insert_xml_fragment(yjs::FRAGMENT);
        let mut txn = doc.transact_mut();
        let have = yjs::read_each(&txn, &fragment);
        let (same_start, same_end) = unchanged_ends(&have, &wanted);
        let removed = have.len() - same_start - same_end;
        if removed > 0 {
            fragment.remove_range(&mut txn, same_start as u32, removed as u32);
        }
        yjs::insert_blocks(
            &mut txn,
            &fragment,
            same_start as u32,
            &wanted[same_start..wanted.len() - same_end],
        );
        Ok(())
    }

    fn write(&self, doc: &Doc) -> Result<Vec<u8>, FormatError> {
        let fragment = doc.get_or_insert_xml_fragment(yjs::FRAGMENT);
        let txn = doc.transact();
        let blocks = yjs::read(&txn, &fragment);
        let state = txn.encode_state_as_update_v1(&StateVector::default());
        Ok(html::page(&blocks, Some(&state)).into_bytes())
    }
}

/// How many blocks at the start, and then at the end, are the same in both.
fn unchanged_ends(have: &[Option<Block>], wanted: &[Block]) -> (usize, usize) {
    let same = |have: &Option<Block>, wanted: &Block| have.as_ref() == Some(wanted);
    let start = have
        .iter()
        .zip(wanted)
        .take_while(|(have, wanted)| same(have, wanted))
        .count();
    let end = have[start..]
        .iter()
        .rev()
        .zip(wanted[start..].iter().rev())
        .take_while(|(have, wanted)| same(have, wanted))
        .count();
    (start, end)
}

/// The file for a new, empty document. It has no state yet: the first
/// person to open it makes one from the body.
pub(crate) fn new_document() -> Vec<u8> {
    html::page(&[Block::Paragraph(Vec::new())], None).into_bytes()
}

#[cfg(test)]
mod tests {
    use yrs::updates::decoder::Decode;
    use yrs::{OffsetKind, Options, Update, XmlFragmentRef};

    use super::*;
    use crate::model::{Inline, Marks};

    fn doc() -> Doc {
        Doc::with_options(Options {
            offset_kind: OffsetKind::Bytes,
            ..Options::default()
        })
    }

    fn paragraph(text: &str) -> Block {
        Block::Paragraph(vec![Inline::Text(text.into(), Marks::default())])
    }

    fn blocks(doc: &Doc) -> Vec<Block> {
        let fragment: XmlFragmentRef = doc.get_or_insert_xml_fragment(yjs::FRAGMENT);
        yjs::read(&doc.transact(), &fragment)
    }

    #[test]
    fn a_document_survives_the_file() {
        let doc = doc();
        let file = html::page(
            &[
                Block::Heading(1, vec![Inline::Text("Plan".into(), Marks::default())]),
                Block::Paragraph(vec![
                    Inline::Text(
                        "bold".into(),
                        Marks {
                            bold: true,
                            ..Marks::default()
                        },
                    ),
                    Inline::Text(" plain".into(), Marks::default()),
                    Inline::Break,
                    Inline::Text(
                        "linked".into(),
                        Marks {
                            link: Some("https://example.com".into()),
                            ..Marks::default()
                        },
                    ),
                ]),
                Block::Checks(vec![(true, vec![paragraph("Milk")])]),
            ],
            None,
        );
        DocFormat.absorb(&doc, file.as_bytes()).unwrap();
        let written = DocFormat.write(&doc).unwrap();
        assert_eq!(html::read(&written), html::read(file.as_bytes()));

        // The state in the file makes the same document elsewhere.
        let state = DocFormat.state_in(&written).unwrap();
        let other = self::doc();
        other
            .transact_mut()
            .apply_update(Update::decode_v1(&state).unwrap())
            .unwrap();
        assert_eq!(blocks(&other), blocks(&doc));
        // And reading its own file back changes nothing.
        let before = doc.transact().state_vector();
        DocFormat.absorb(&doc, &written).unwrap();
        assert_eq!(doc.transact().state_vector(), before);
    }

    #[test]
    fn a_change_outside_touches_only_its_blocks() {
        let doc = doc();
        let first = html::page(
            &[paragraph("One"), paragraph("Two"), paragraph("Three")],
            None,
        );
        DocFormat.absorb(&doc, first.as_bytes()).unwrap();
        let fragment = doc.get_or_insert_xml_fragment(yjs::FRAGMENT);
        let first_id = fragment.get(&doc.transact(), 0).unwrap().id();
        let last_id = fragment.get(&doc.transact(), 2).unwrap().id();

        let changed = html::page(
            &[paragraph("One"), paragraph("2"), paragraph("Three")],
            None,
        );
        DocFormat.absorb(&doc, changed.as_bytes()).unwrap();
        assert_eq!(blocks(&doc), html::read(changed.as_bytes()));
        // The blocks that didn't change are the same ones.
        assert_eq!(fragment.get(&doc.transact(), 0).unwrap().id(), first_id);
        assert_eq!(fragment.get(&doc.transact(), 2).unwrap().id(), last_id);
    }

    #[test]
    fn a_new_document_opens_with_one_paragraph() {
        let doc = doc();
        let file = new_document();
        assert!(html::is_document(&file));
        assert_eq!(DocFormat.state_in(&file), None);
        DocFormat.absorb(&doc, &file).unwrap();
        assert_eq!(blocks(&doc), vec![Block::Paragraph(Vec::new())]);
    }
}
