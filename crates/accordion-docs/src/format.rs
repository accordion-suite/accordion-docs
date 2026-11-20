//! A document as core's co-editing service sees it (decision 30): the file
//! is HTML with the co-editing state in its data block, and the document
//! is y-prosemirror's XML fragment.
//!
//! Core keeps the state between saves and merges a change made outside
//! Docs three ways, from the state at the last save. This format only has to
//! read and write files, and make a document match a file with few edits.

use accordion_core::coedit::{Format, FormatError};
use yrs::updates::decoder::Decode;
use yrs::{Doc, OffsetKind, Options, ReadTxn, StateVector, Transact, Update, XmlFragment};

use crate::html;
use crate::images::Images;
use crate::model::{self, Block, PageSetup};
use crate::picture::Pictures;
use crate::yjs;

pub(crate) struct DocFormat {
    /// Where the pictures are, since the editor's document has only their
    /// hashes.
    pub images: Images,
}

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
        let html::Document {
            blocks: wanted,
            setup,
            pictures,
        } = html::read_document(contents);
        self.images.remember(pictures);
        let fragment = doc.get_or_insert_xml_fragment(yjs::FRAGMENT);
        let page = doc.get_or_insert_map(yjs::PAGE);
        let mut txn = doc.transact_mut();
        if yjs::read_page(&txn, &page) != setup {
            yjs::write_page(&mut txn, &page, setup.as_ref());
        }
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
        let page = doc.get_or_insert_map(yjs::PAGE);
        let txn = doc.transact();
        let blocks = yjs::read(&txn, &fragment);
        let setup = yjs::read_page(&txn, &page);
        let state = txn.encode_state_as_update_v1(&StateVector::default());
        drop(txn);
        let pictures = self.images.for_saving(&model::image_hashes(&blocks));
        Ok(html::page(&blocks, setup.as_ref(), Some(&state), &pictures).into_bytes())
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

impl DocFormat {
    /// The file for a new document whose first changes were made in a
    /// browser before it had a file: `update` is the browser's whole state,
    /// kept as the file's, so the browser joins it without merging its
    /// changes in twice. `setup` is the paper if the state has none.
    /// Nothing if `update` isn't a co-editing state.
    pub(crate) fn file_from_draft(&self, update: &[u8], setup: &PageSetup) -> Option<Vec<u8>> {
        let update = Update::decode_v1(update).ok()?;
        let doc = Doc::with_options(Options {
            offset_kind: OffsetKind::Bytes,
            ..Options::default()
        });
        let page = doc.get_or_insert_map(yjs::PAGE);
        doc.get_or_insert_xml_fragment(yjs::FRAGMENT);
        {
            let mut txn = doc.transact_mut();
            txn.apply_update(update).ok()?;
            // Parts it depends on, which a whole state never lacks.
            if txn.store().pending_update().is_some() || txn.store().pending_ds().is_some() {
                return None;
            }
            if yjs::read_page(&txn, &page).is_none() {
                yjs::write_page(&mut txn, &page, Some(setup));
            }
        }
        self.write(&doc).ok()
    }
}

/// The file for a new, empty document on `setup`'s page. It has no state
/// yet: the first person to open it makes one from the body.
pub(crate) fn new_document(setup: &PageSetup) -> Vec<u8> {
    html::page(
        &[Block::Paragraph(model::Para::default(), Vec::new())],
        Some(setup),
        None,
        &Pictures::new(),
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use yrs::updates::decoder::Decode;
    use yrs::{OffsetKind, Options, Update, XmlFragmentRef};

    use super::*;
    use crate::model::{Image, Inline, Marks, PageSize, Script};
    use crate::picture::Picture;
    use crate::style::Color;

    fn format() -> DocFormat {
        DocFormat {
            images: Images::in_memory(),
        }
    }

    fn doc() -> Doc {
        Doc::with_options(Options {
            offset_kind: OffsetKind::Bytes,
            ..Options::default()
        })
    }

    fn paragraph(text: &str) -> Block {
        Block::Paragraph(
            model::Para::default(),
            vec![Inline::Text(text.into(), Marks::default())],
        )
    }

    fn page(doc: &Doc) -> Option<PageSetup> {
        let page = doc.get_or_insert_map(yjs::PAGE);
        yjs::read_page(&doc.transact(), &page)
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
                Block::Heading(
                    1,
                    model::Para::default(),
                    vec![Inline::Text("Plan".into(), Marks::default())],
                ),
                Block::Paragraph(
                    model::Para::default(),
                    vec![
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
                        Inline::Text(
                            "styled".into(),
                            Marks {
                                color: Some(Color([0xcc, 0, 0])),
                                highlight: Some(Color([0xff, 0xf2, 0xcc])),
                                size: Some(21),
                                font: Some("Open Sans".into()),
                                script: Some(Script::Super),
                                ..Marks::default()
                            },
                        ),
                    ],
                ),
                Block::Checks(vec![(true, vec![paragraph("Milk")])]),
            ],
            Some(&PageSetup::new(PageSize::A4)),
            None,
            &Pictures::new(),
        );
        format().absorb(&doc, file.as_bytes()).unwrap();
        let written = format().write(&doc).unwrap();
        assert_eq!(html::read(&written), html::read(file.as_bytes()));

        // The state in the file makes the same document elsewhere.
        let state = format().state_in(&written).unwrap();
        let other = self::doc();
        other
            .transact_mut()
            .apply_update(Update::decode_v1(&state).unwrap())
            .unwrap();
        assert_eq!(blocks(&other), blocks(&doc));
        assert_eq!(page(&other), Some(PageSetup::new(PageSize::A4)));
        // And reading its own file back changes nothing.
        let before = doc.transact().state_vector();
        format().absorb(&doc, &written).unwrap();
        assert_eq!(doc.transact().state_vector(), before);
    }

    #[test]
    fn layout_survives_the_editor_and_the_file() {
        let doc = doc();
        let blocks = html::tests::laid_out();
        let file = html::page(&blocks, None, None, &Pictures::new());
        format().absorb(&doc, file.as_bytes()).unwrap();
        assert_eq!(self::blocks(&doc), blocks);
        let written = format().write(&doc).unwrap();
        assert_eq!(html::read(&written), blocks);
    }

    #[test]
    fn cell_colors_survive_the_editor_and_the_file() {
        let doc = doc();
        let blocks = html::tests::colored_cells();
        let file = html::page(&blocks, None, None, &Pictures::new());
        format().absorb(&doc, file.as_bytes()).unwrap();
        assert_eq!(self::blocks(&doc), blocks);
        assert_eq!(html::read(&format().write(&doc).unwrap()), blocks);
    }

    #[test]
    fn a_draft_becomes_a_file_with_its_own_state() {
        // A browser typed into a document that had no file yet.
        let draft = doc();
        {
            let fragment = draft.get_or_insert_xml_fragment(yjs::FRAGMENT);
            let mut txn = draft.transact_mut();
            yjs::insert_blocks(&mut txn, &fragment, 0, &[paragraph("First words")]);
        }
        let update = draft
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        let file = format()
            .file_from_draft(&update, &PageSetup::new(PageSize::A4))
            .unwrap();
        let read = html::read_document(&file);
        assert_eq!(read.blocks, vec![paragraph("First words")]);
        assert_eq!(read.setup, Some(PageSetup::new(PageSize::A4)));

        // The browser's state is the file's, so joining adds nothing twice.
        let joined = doc();
        joined
            .transact_mut()
            .apply_update(Update::decode_v1(&format().state_in(&file).unwrap()).unwrap())
            .unwrap();
        joined
            .transact_mut()
            .apply_update(Update::decode_v1(&update).unwrap())
            .unwrap();
        assert_eq!(blocks(&joined), vec![paragraph("First words")]);

        // A paper the browser chose wins over the default.
        {
            let page = draft.get_or_insert_map(yjs::PAGE);
            let mut txn = draft.transact_mut();
            yjs::write_page(&mut txn, &page, Some(&PageSetup::new(PageSize::Legal)));
        }
        let update = draft
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        let file = format()
            .file_from_draft(&update, &PageSetup::new(PageSize::A4))
            .unwrap();
        assert_eq!(
            html::read_document(&file).setup,
            Some(PageSetup::new(PageSize::Legal))
        );

        assert_eq!(
            format().file_from_draft(b"not a state", &PageSetup::new(PageSize::A4)),
            None
        );
    }

    #[tokio::test]
    async fn saving_writes_each_picture_into_the_file() {
        let format = format();
        let doc = doc();
        // Someone added a picture, and the editor refers to it by hash.
        let picture = Picture::read(crate::picture::tests::png(40, 30)).unwrap();
        format.images.add(picture.clone()).await.unwrap();
        let image = Block::Image(Image {
            hash: picture.hash.clone(),
            alt: "A dot".into(),
            width: Some(20),
        });
        {
            let fragment = doc.get_or_insert_xml_fragment(yjs::FRAGMENT);
            let mut txn = doc.transact_mut();
            yjs::insert_blocks(&mut txn, &fragment, 0, &[paragraph("Look"), image.clone()]);
        }
        let written = format.write(&doc).unwrap();
        let text = String::from_utf8(written.clone()).unwrap();
        assert!(text.contains(&picture.data_address()), "{text}");
        let read = html::read_document(&written);
        assert_eq!(read.blocks, vec![paragraph("Look"), image]);
        assert_eq!(read.pictures.get(&picture.hash), Some(&picture));

        // Another server opening the file has the picture from it.
        let elsewhere = self::format();
        let other = self::doc();
        elsewhere.absorb(&other, &written).unwrap();
        assert!(
            String::from_utf8(elsewhere.write(&other).unwrap())
                .unwrap()
                .contains(&picture.data_address())
        );
    }

    #[test]
    fn a_page_setup_changed_outside_comes_in() {
        let doc = doc();
        let blocks = [paragraph("One")];
        let a4 = PageSetup::new(PageSize::A4);
        format()
            .absorb(
                &doc,
                html::page(&blocks, Some(&a4), None, &Pictures::new()).as_bytes(),
            )
            .unwrap();
        let mut sideways = a4;
        sideways.landscape = true;
        sideways.margins[1] = 36.0;
        format()
            .absorb(
                &doc,
                html::page(&blocks, Some(&sideways), None, &Pictures::new()).as_bytes(),
            )
            .unwrap();
        assert_eq!(page(&doc), Some(sideways));
        // A file without one, such as an older version, has none.
        format()
            .absorb(
                &doc,
                html::page(&blocks, None, None, &Pictures::new()).as_bytes(),
            )
            .unwrap();
        assert_eq!(page(&doc), None);
        assert!(
            !String::from_utf8(format().write(&doc).unwrap())
                .unwrap()
                .contains("@page")
        );
    }

    #[test]
    fn a_change_outside_touches_only_its_blocks() {
        let doc = doc();
        let first = html::page(
            &[paragraph("One"), paragraph("Two"), paragraph("Three")],
            None,
            None,
            &Pictures::new(),
        );
        format().absorb(&doc, first.as_bytes()).unwrap();
        let fragment = doc.get_or_insert_xml_fragment(yjs::FRAGMENT);
        let first_id = fragment.get(&doc.transact(), 0).unwrap().id();
        let last_id = fragment.get(&doc.transact(), 2).unwrap().id();

        let changed = html::page(
            &[paragraph("One"), paragraph("2"), paragraph("Three")],
            None,
            None,
            &Pictures::new(),
        );
        format().absorb(&doc, changed.as_bytes()).unwrap();
        assert_eq!(blocks(&doc), html::read(changed.as_bytes()));
        // The blocks that didn't change are the same ones.
        assert_eq!(fragment.get(&doc.transact(), 0).unwrap().id(), first_id);
        assert_eq!(fragment.get(&doc.transact(), 2).unwrap().id(), last_id);
    }

    #[test]
    fn a_new_document_opens_with_one_paragraph() {
        let doc = doc();
        let file = new_document(&PageSetup::new(PageSize::A4));
        assert!(html::is_document(&file));
        assert_eq!(format().state_in(&file), None);
        format().absorb(&doc, &file).unwrap();
        assert_eq!(
            blocks(&doc),
            vec![Block::Paragraph(model::Para::default(), Vec::new())]
        );
        assert_eq!(page(&doc), Some(PageSetup::new(PageSize::A4)));
    }
}
