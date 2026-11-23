# Accordion Docs

Docs for Accordion: documents edited together, live, as in Google Docs. Each document is an HTML file in Drive that any browser opens. It has fonts, colors, headings, lists, checklists, tables, pictures, page breaks, headers, footers, page numbers, footnotes, and a table of contents, prints on the paper size you choose, and downloads as a web page, Word, OpenDocument, or Markdown file. Opening Word files, comments, version history, and templates are planned.

This repo is part of [Accordion](https://github.com/accordion-suite/accordion), a self-hosted, zero-telemetry replacement for Google Workspace. Please open issues in the [main repo](https://github.com/accordion-suite/accordion/issues).

## How it fits together

Docs is a library crate, `accordion-docs`, that the server in the `accordion` repo builds in. It implements the `Module` trait from `accordion-module` ([decision 7](https://github.com/accordion-suite/accordion/blob/main/docs/decisions/0007-module-interface.md)), and requires Drive. [Decision 30](https://github.com/accordion-suite/accordion/blob/main/docs/decisions/0030-documents-and-co-editing.md) explains the design and the order features are built in. The editor is in the `accordion-web` repo.

Each document is an HTML file in its owner's Docs place, which Drive shows as a tab beside My Drive ([decision 32](https://github.com/accordion-suite/accordion/blob/main/docs/decisions/0032-a-place-for-each-apps-files.md)):

- **An ordinary page -** The file has a small stylesheet for screen and print, and no scripts, so it opens read-only in any browser, synced to a computer or downloaded. It follows the browser's dark mode on screen, unless the document has colored text or cells, and always prints dark on white.
- **It carries its own co-editing state** in a data block, `<script type="application/x-accordion-docs">`, which browsers neither run nor show. A document copied, synced, or restored from a backup still opens in Docs with its edits.
- **Docs knows a document by how it starts:** `<!DOCTYPE html>` then `<html data-accordion-docs="1"`. Other HTML files stay web pages.
- **The server writes the body from the schema** in `src/model.rs`, so only its elements and attributes reach the file: never a script or an event handler. Styles reach it only as the schema writes them, from values `src/style.rs` checks first, such as a color of six hex digits. HTML from other programs, such as what's pasted from Google Docs or Word, is read through the same schema.
- **Fonts are always ones Docs has, by their own names** (`src/fonts.rs`, the same list as the web app's `src/docs/fonts.ts`). A font Docs doesn't have becomes one it has as it's read: Arimo for Arial, with the same letter widths, Noto Sans for Verdana, which looks like it, and so on. Weights other than regular and bold are their own mark, and Word downloads name them in the font, such as "Raleway Light".
- **Page setup is part of the document:** the paper size, orientation, and margins are on the file's `<main>` element and in its `@page` style. Headers and footers are `<header data-part>` and `<footer data-part>` in `<main>`.
- **Footnotes are in the text they belong to:** in the editor and in Yjs, a footnote holds its note, so it moves and copies with its number. The file shows each number as a link, and the notes in a `<section data-footnotes>` at the end of `<main>`, as GitHub writes them, and reads GitHub's footnotes the same way.
- **A table of contents keeps only its style:** `<nav data-contents>` in the file and `table_of_contents` in Yjs. Its entries are the document's headings, found again each time it's shown or written (`model::contents`), so it's never out of date. The file links each entry to its heading's `id`, named as GitHub names Markdown headings.
- **Pictures are in the file,** as `data:` addresses, so the file stays whole on its own. The server shrinks each picture people add to 2,560 pixels on its longer side, in the sandbox, and leaves out its metadata. While documents are open, Docs keeps pictures in its `docs_images` table by their SHA-256 hash (`src/images.rs`).
- **Editing together** uses core's co-editing service ([decision 30](https://github.com/accordion-suite/accordion/blob/main/docs/decisions/0030-documents-and-co-editing.md)). The document is the Yjs XML fragment that y-prosemirror binds the editor to, laid out the way y-prosemirror lays it out (`src/yjs.rs`), with page setup in a Yjs map beside it. When the file changes outside Docs, only the blocks that differ change, so people typing elsewhere keep their typing.
- **Downloads are written straight from the schema** (`src/docx.rs`, `src/odt.rs`, and `src/markdown.rs`), with the document's paper and margins, so they don't need LibreOffice. Footnotes and tables of contents are each format's own. A table of contents gets the page numbers the editor shows, which the download address carries as `?pages=`. PDF comes from the browser, which prints the editor's own pages.
- **Open with Docs makes a document from a file** (`src/import/`): a Word file, a web page, Markdown, or plain text. Docs reads Word files itself (`src/import/word.rs`), with their styles, theme fonts, lists, tables, footnotes, headers, footers, fields, and page setup, so it needs no LibreOffice. The file is untrusted, so the server's own program reads it in the sandbox as `docs-import`, and the server reads the page it writes through the schema again and shrinks its pictures. A report says what didn't come across, such as text boxes or tracked changes, which the web app shows before converting.

| Path | What it's for | How it signs in |
|---|---|---|
| `/api/docs/` | The JSON API, described in `src/api.rs`: listing, making, opening, and copying documents | Session cookie or bearer token |
| `/api/docs/documents/{id}/live` | A WebSocket for editing a document together, with Yjs's sync and awareness messages | Session cookie or bearer token, and a matching `Origin` from a browser |
| `/api/docs/documents/{id}/download?as=` | The document as `html`, `docx`, `odt`, or `md` | Session cookie or bearer token |
| `/api/docs/documents/{id}/images` | Adding a picture, up to 40 MB, for editors | Session cookie or bearer token |

## Building

Install Rust with [rustup](https://rustup.rs), then run these commands from the repo root:

```sh
cargo build --workspace
cargo test --workspace
```

The picture tests shrink a photo in the sandbox, with `accordion-docs-sandbox` as the launcher, so they need a Linux kernel with Landlock and libvips, as `accordion-core`'s tests do.

To build against your own checkout of `accordion-core`, clone the workspace with `scripts/clone-workspace.sh` in the `accordion` repo. Before you commit `Cargo.lock`, push `accordion-core` and run `scripts/pin-versions.sh ../accordion-docs` from the `accordion` repo.

## License

[AGPL-3.0 or later](LICENSE)
