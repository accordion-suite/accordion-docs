# Accordion Docs

Docs for Accordion: documents with headings, lists, checklists, and tables, edited together live, as in Google Docs. Each document is an HTML file in Drive that any browser opens. Images, comments, import from Word, and version history are planned.

This repo is part of [Accordion](https://github.com/accordion-suite/accordion), a self-hosted, zero-telemetry replacement for Google Workspace. Please open issues in the [main repo](https://github.com/accordion-suite/accordion/issues).

## How it fits together

Docs is a library crate, `accordion-docs`, that the server in the `accordion` repo builds in. It implements the `Module` trait from `accordion-module` ([decision 7](https://github.com/accordion-suite/accordion/blob/main/docs/decisions/0007-module-interface.md)), and requires Drive. [Decision 30](https://github.com/accordion-suite/accordion/blob/main/docs/decisions/0030-documents-and-co-editing.md) explains the design. The editor is in the `accordion-web` repo.

Each document is an HTML file in its owner's Docs place, which Drive shows as a tab beside My Drive ([decision 32](https://github.com/accordion-suite/accordion/blob/main/docs/decisions/0032-a-place-for-each-apps-files.md)):

- **An ordinary page.** The file has a small stylesheet for screen and print, and no scripts, so it opens read-only in any browser, synced to a computer or downloaded.
- **It carries its own co-editing state** in a data block, `<script type="application/x-accordion-docs">`, which browsers neither run nor show. A document copied, synced, or restored from a backup still opens in Docs with its edits.
- **Docs knows a document by how it starts:** `<!DOCTYPE html>` then `<html data-accordion-docs="1"`. Other HTML files stay web pages.
- **The server writes the body from the schema** in `src/model.rs`, so only its elements and attributes reach the file: never a script, a style attribute, or an event handler. HTML from other programs is read through the same schema.
- **Editing together** uses core's co-editing service ([decision 30](https://github.com/accordion-suite/accordion/blob/main/docs/decisions/0030-documents-and-co-editing.md)). The document is the Yjs XML fragment that y-prosemirror binds the editor to, laid out the way y-prosemirror lays it out (`src/yjs.rs`). When the file changes outside Docs, only the blocks that differ change, so people typing elsewhere keep their typing.

| Path | What it's for | How it signs in |
|---|---|---|
| `/api/docs/` | The JSON API, described in `src/api.rs` | Session cookie or bearer token |
| `/api/docs/documents/{id}/live` | A WebSocket for editing a document together, with Yjs's sync and awareness messages | Session cookie or bearer token, and a matching `Origin` from a browser |

## Building

Install Rust with [rustup](https://rustup.rs), then run these commands from the repo root:

```sh
cargo build --workspace
cargo test --workspace
```

To build against your own checkout of `accordion-core`, clone the workspace with `scripts/clone-workspace.sh` in the `accordion` repo. Before you commit `Cargo.lock`, push `accordion-core` and run `scripts/pin-versions.sh ../accordion-docs` from the `accordion` repo.

## License

[AGPL-3.0 or later](LICENSE)
