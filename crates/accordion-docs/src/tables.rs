//! Docs' tables, and the migrations that make them.

use accordion_core::db::Migration;

/// The owner of Docs' tables.
pub(crate) const OWNER: &str = "docs";

pub(crate) static MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "Keep pictures in documents",
        sql: "
CREATE TABLE docs_images (
    hash TEXT PRIMARY KEY,
    content_type TEXT NOT NULL,
    contents BLOB NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    -- When a document last used it, in seconds since 1970.
    used_at INTEGER NOT NULL
) STRICT;
CREATE INDEX docs_images_used_at ON docs_images (used_at);
",
    },
    Migration {
        version: 2,
        name: "Keep documents' comments",
        sql: "
-- Each document's comment threads, as the JSON its file carries. A row
-- means the file's comments were read in, so an empty list is a document
-- whose comments were all deleted.
CREATE TABLE docs_comments (
    item TEXT PRIMARY KEY,
    threads TEXT NOT NULL
) STRICT;
",
    },
];
