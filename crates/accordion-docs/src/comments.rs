//! Comments on documents (decision 30, step 6).
//!
//! A thread is about a stretch of text, which two Yjs relative positions
//! mark. The editor makes them and reads them: they name the characters
//! themselves, so a thread stays with its text as people edit around it,
//! and the server keeps them as they come, in base64.
//!
//! Threads are kept in `docs_comments` while Docs has the document, and in
//! the file's data block, so they travel with it: a copy, a file synced to
//! a computer, or one moved to another server keeps them. The table takes
//! them in from the file the first time a document's comments are needed,
//! and from then on the table wins, so restoring an older version of the
//! file neither brings back deleted comments nor takes away new ones, as in
//! Google Docs.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use accordion_core::db::rusqlite::{self, OptionalExtension};
use accordion_core::db::{self, Database};
use accordion_core::events::Actor;
use accordion_core::storage::{self, ItemKind, Storage};
use accordion_module::ApiError;
use accordion_types::{ItemId, UserId};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use crate::{html, tables};

/// The most threads one document keeps, far more than a document has.
pub const MAX_THREADS: usize = 1000;
/// The most comments in one thread, the first one and its replies.
pub const MAX_COMMENTS: usize = 200;
/// The longest comment, in characters.
pub const MAX_TEXT: usize = 4000;
/// How much of the commented text a thread keeps, in characters, to show
/// beside it once the text is gone.
pub const MAX_QUOTE: usize = 200;
/// The longest position, in base64. Yjs's are a few dozen characters.
pub const MAX_ANCHOR: usize = 512;
/// The longest ID a file's thread or comment may have.
const MAX_ID: usize = 64;
/// The largest file whose comments are written into it, as core's
/// co-editing limit.
const MAX_FILE: u64 = 50 * 1024 * 1024;

/// A comment and its replies, about one stretch of text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    /// Where the text starts and ends, as Yjs relative positions in base64.
    pub start: String,
    pub end: String,
    /// The text as it was when the thread started.
    #[serde(default)]
    pub quote: String,
    /// Who resolved it and when, while it's resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<Mark>,
    /// The first comment, then the replies, oldest first.
    pub comments: Vec<Comment>,
}

/// Who did something, and when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    /// Their account, on the server they did it on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<UserId>,
    /// Their name then, for a file on another server.
    #[serde(default)]
    pub name: String,
    /// When, in RFC 3339.
    pub at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    /// The author's account, on the server they wrote it on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<UserId>,
    /// The author's name then, for a file on another server.
    #[serde(default)]
    pub name: String,
    pub text: String,
    /// When it was written, in RFC 3339.
    pub at: String,
    /// When it was last changed, if it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited: Option<String>,
}

/// A new ID for a thread or a comment.
pub fn new_id() -> String {
    Uuid::now_v7().simple().to_string()
}

/// Now, in RFC 3339, to the millisecond.
pub fn now() -> String {
    jiff::Timestamp::now()
        .round(jiff::Unit::Millisecond)
        .map(|now| now.to_string())
        .unwrap_or_else(|_| jiff::Timestamp::now().to_string())
}

/// Whether `anchor` is a position the server keeps: base64, and short.
pub fn is_anchor(anchor: &str) -> bool {
    !anchor.is_empty()
        && anchor.len() <= MAX_ANCHOR
        && base64::engine::general_purpose::STANDARD
            .decode(anchor)
            .is_ok()
}

/// `text` cut to `most` characters.
pub fn shortened(text: &str, most: usize) -> String {
    text.chars().take(most).collect()
}

/// Threads read from a file, kept to what Docs itself writes: IDs that are
/// there and different, positions it can keep, texts no longer than a
/// comment may be, and no more than a document may have.
pub fn checked(threads: Vec<Thread>) -> Vec<Thread> {
    let fits = |id: &str| !id.is_empty() && id.len() <= MAX_ID;
    let mut seen = HashSet::new();
    let mut kept = Vec::new();
    for mut thread in threads {
        if kept.len() == MAX_THREADS {
            break;
        }
        if !fits(&thread.id)
            || !seen.insert(thread.id.clone())
            || !is_anchor(&thread.start)
            || !is_anchor(&thread.end)
        {
            continue;
        }
        thread.quote = shortened(&thread.quote, MAX_QUOTE);
        if let Some(mark) = &mut thread.resolved {
            mark.name = shortened(&mark.name, MAX_QUOTE);
        }
        let mut ids = HashSet::new();
        thread
            .comments
            .retain(|comment| fits(&comment.id) && ids.insert(comment.id.clone()));
        thread.comments.truncate(MAX_COMMENTS);
        for comment in &mut thread.comments {
            comment.text = shortened(&comment.text, MAX_TEXT);
            comment.name = shortened(&comment.name, MAX_QUOTE);
        }
        if !thread.comments.is_empty() {
            kept.push(thread);
        }
    }
    kept
}

/// Why a change to a document's comments didn't happen.
pub(crate) enum Problem {
    Sql(rusqlite::Error),
    /// The change isn't allowed, or doesn't fit, with a message for the
    /// person.
    Refused(ApiError),
}

impl From<rusqlite::Error> for Problem {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sql(err)
    }
}

impl From<Problem> for ApiError {
    fn from(problem: Problem) -> Self {
        match problem {
            Problem::Refused(err) => err,
            Problem::Sql(err) => {
                tracing::error!("couldn't keep documents' comments: {err}");
                ApiError::server_problem()
            }
        }
    }
}

/// Every document's comments.
#[derive(Clone)]
pub(crate) struct Comments {
    db: Database,
    /// One write of comments into each file at a time, and the ID that
    /// makes a document's writes one version, as a co-editing session's are.
    writing: Arc<Mutex<HashMap<ItemId, Arc<tokio::sync::Mutex<Uuid>>>>>,
}

fn parse(item: ItemId, json: &str) -> Vec<Thread> {
    serde_json::from_str(json).unwrap_or_else(|err| {
        tracing::error!(%item, %err, "a document's kept comments aren't readable");
        Vec::new()
    })
}

fn read(conn: &rusqlite::Connection, item: ItemId) -> rusqlite::Result<Option<Vec<Thread>>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT threads FROM docs_comments WHERE item = ?1",
            [item.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(json.map(|json| parse(item, &json)))
}

impl Comments {
    pub async fn open(db: Database) -> Result<Self, db::Error> {
        db.migrate(tables::OWNER, tables::MIGRATIONS).await?;
        Ok(Self {
            db,
            writing: Arc::default(),
        })
    }

    /// The document's threads, or `None` if they haven't been taken in from
    /// its file yet.
    pub async fn get(&self, item: ItemId) -> Result<Option<Vec<Thread>>, db::Error> {
        self.db.read(move |conn| Ok(read(conn, item)?)).await
    }

    /// The same as [`Comments::get`], for a co-editing format's `write`,
    /// which can't wait on a future.
    pub fn for_saving(&self, item: ItemId) -> Result<Option<Vec<Thread>>, db::Error> {
        self.db.read_blocking(move |conn| Ok(read(conn, item)?))
    }

    /// Keeps `threads`, from the document's file, unless the document's
    /// comments are already here.
    pub async fn take_in(&self, item: ItemId, threads: Vec<Thread>) -> Result<(), db::Error> {
        let json = serde_json::to_string(&threads).expect("threads become JSON");
        self.db
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO docs_comments (item, threads) VALUES (?1, ?2)
                     ON CONFLICT (item) DO NOTHING",
                    (item.to_string(), json),
                )
            })
            .await?;
        Ok(())
    }

    /// Changes the document's threads with `change`, which can refuse. The
    /// threads must have been taken in.
    pub async fn change<T, F>(&self, item: ItemId, change: F) -> Result<T, Problem>
    where
        F: FnOnce(&mut Vec<Thread>) -> Result<T, ApiError> + Send + 'static,
        T: Send + 'static,
    {
        self.db
            .write(move |tx| {
                let mut threads = read(tx, item)?.unwrap_or_default();
                let done = change(&mut threads).map_err(Problem::Refused)?;
                let json = serde_json::to_string(&threads).expect("threads become JSON");
                tx.execute(
                    "INSERT INTO docs_comments (item, threads) VALUES (?1, ?2)
                     ON CONFLICT (item) DO UPDATE SET threads = excluded.threads",
                    (item.to_string(), json),
                )?;
                Ok(done)
            })
            .await
    }

    /// Writes the document's threads into its file, so the file carries
    /// them. While the document is open, core's co-editing service sees the
    /// file change, and finds nothing in it to take in. If the file can't
    /// be written, the threads are still kept here, and the next save of
    /// the document writes them.
    pub async fn write_file(&self, storage: &Storage, actor: Actor, item: ItemId) {
        let lock = Arc::clone(
            self.writing
                .lock()
                .expect("never poisoned")
                .entry(item)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(Uuid::now_v7()))),
        );
        let edit = lock.lock().await;
        // Someone else's save can land between reading and writing, so
        // it's read again then.
        for _ in 0..3 {
            let blob = match storage.item(item).await {
                Ok(Some(found)) => match found.kind {
                    ItemKind::File { blob, .. } => blob,
                    ItemKind::Folder => return,
                },
                Ok(None) => return,
                Err(err) => {
                    tracing::warn!(%item, %err, "couldn't find a document to write its comments in");
                    return;
                }
            };
            if blob.size > MAX_FILE {
                return;
            }
            let mut contents = Vec::new();
            let read = match storage.open_contents(blob.hash).await {
                Ok(file) => file.take(MAX_FILE).read_to_end(&mut contents).await,
                Err(err) => {
                    tracing::warn!(%item, %err, "couldn't read a document to write its comments in");
                    return;
                }
            };
            if let Err(err) = read {
                tracing::warn!(%item, %err, "couldn't read a document to write its comments in");
                return;
            }
            let threads = match self.get(item).await {
                Ok(Some(threads)) => threads,
                Ok(None) => return,
                Err(err) => {
                    tracing::warn!(%item, %err, "couldn't read a document's comments");
                    return;
                }
            };
            let Some(written) = html::with_comments(&contents, &threads) else {
                return;
            };
            if written == contents {
                return;
            }
            match storage
                .acting_as(actor)
                .write_edit(item, blob.hash, *edit, &written[..])
                .await
            {
                Ok(_) => return,
                Err(storage::Error::Changed) => continue,
                Err(err) => {
                    tracing::warn!(%item, %err, "couldn't write a document's comments in its file");
                    return;
                }
            }
        }
    }

    /// Lets go of the comments of documents deleted for good. Returns how
    /// many.
    pub async fn tidy(&self, storage: &Storage) -> Result<usize, db::Error> {
        let items: Vec<String> = self
            .db
            .read(|conn| {
                let mut statement = conn.prepare("SELECT item FROM docs_comments")?;
                statement
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()
            })
            .await?;
        let mut gone = Vec::new();
        for text in items {
            let Ok(item) = text.parse::<ItemId>() else {
                gone.push(text);
                continue;
            };
            if matches!(storage.item(item).await, Ok(None)) {
                gone.push(text);
            }
        }
        let count = gone.len();
        if count > 0 {
            self.db
                .write(move |tx| {
                    for item in gone {
                        tx.execute("DELETE FROM docs_comments WHERE item = ?1", [item])?;
                    }
                    Ok::<_, rusqlite::Error>(())
                })
                .await?;
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(id: &str, text: &str) -> Comment {
        Comment {
            id: id.into(),
            author: None,
            name: "Dana".into(),
            text: text.into(),
            at: "2026-11-25T10:00:00Z".into(),
            edited: None,
        }
    }

    fn thread(id: &str, comments: Vec<Comment>) -> Thread {
        Thread {
            id: id.into(),
            start: "AQ==".into(),
            end: "Ag==".into(),
            quote: "plan".into(),
            resolved: None,
            comments,
        }
    }

    #[test]
    fn keeps_only_what_docs_writes_from_a_file() {
        let long = "x".repeat(MAX_TEXT + 10);
        let mut bad_anchor = thread("b", vec![comment("1", "hi")]);
        bad_anchor.start = "not base64!".into();
        let kept = checked(vec![
            thread(
                "a",
                vec![
                    comment("1", "first"),
                    comment("1", "same ID"),
                    comment("2", &long),
                ],
            ),
            thread("a", vec![comment("3", "same thread ID")]),
            bad_anchor,
            thread("c", Vec::new()),
            thread("", vec![comment("4", "no ID")]),
        ]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].comments.len(), 2);
        assert_eq!(kept[0].comments[0].text, "first");
        assert_eq!(kept[0].comments[1].text.chars().count(), MAX_TEXT);
    }

    #[test]
    fn makes_new_ids_and_times() {
        assert_ne!(new_id(), new_id());
        assert_eq!(new_id().len(), 32);
        let now = now();
        assert!(now.ends_with('Z'), "{now}");
        assert!(now.parse::<jiff::Timestamp>().is_ok());
    }
}
