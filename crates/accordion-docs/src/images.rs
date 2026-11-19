//! Where Docs keeps pictures while documents are open (decision 30).
//!
//! A document's file holds each of its pictures in its body, and the
//! editor's document refers to them by hash. Saving writes the file from
//! the editor's document, so every save needs the contents of every
//! picture. They're kept here: in `docs_images`, by hash, and the most
//! recently used in memory.
//!
//! Pictures arrive three ways: someone adds one, a file changes outside
//! Docs and brings new ones, or someone opens a document whose pictures
//! aren't here, because they were left out after months unused or the
//! file came from another server. Opening a document reads its file's
//! pictures in first, so a save never finds one missing.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use accordion_core::db::rusqlite::{self, OptionalExtension};
use accordion_core::db::{self, Database, Migration};

use crate::html;
use crate::picture::{self, Picture, Pictures};

/// The owner of Docs' tables.
const OWNER: &str = "docs";

pub(crate) static MIGRATIONS: &[Migration] = &[Migration {
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
}];

/// Pictures no document has used for this long are left out. Opening a
/// document that has one brings it back from the file.
const UNUSED_FOR: Duration = Duration::from_secs(90 * 24 * 60 * 60);

/// How often a picture's use is noted, at most.
const NOTE_USE_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// How much memory recently used pictures may take.
const CACHE_SIZE: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct Images {
    inner: Arc<Inner>,
}

struct Inner {
    /// `None` in tests that need only memory.
    db: Option<Database>,
    cache: Mutex<Cache>,
    /// When each picture's use was last noted, so it's noted once a day.
    noted: Mutex<HashMap<String, Instant>>,
    /// Files whose pictures are known to be here, by the file's hash.
    files: Mutex<HashSet<String>>,
}

#[derive(Default)]
struct Cache {
    pictures: HashMap<String, (Picture, Instant)>,
    size: usize,
}

impl Cache {
    fn get(&mut self, hash: &str) -> Option<Picture> {
        let (picture, used) = self.pictures.get_mut(hash)?;
        *used = Instant::now();
        Some(picture.clone())
    }

    fn put(&mut self, picture: Picture) {
        if self.pictures.contains_key(&picture.hash) {
            return;
        }
        self.size += picture.contents.len();
        self.pictures
            .insert(picture.hash.clone(), (picture, Instant::now()));
        // The least recently used go first.
        while self.size > CACHE_SIZE && self.pictures.len() > 1 {
            let oldest = self
                .pictures
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(hash, _)| hash.clone())
                .expect("the cache isn't empty");
            if let Some((gone, _)) = self.pictures.remove(&oldest) {
                self.size -= gone.contents.len();
            }
        }
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

fn picture_from_row(row: &rusqlite::Row) -> rusqlite::Result<Option<Picture>> {
    let contents: Vec<u8> = row.get(0)?;
    Ok(Picture::read(contents))
}

impl Images {
    pub async fn open(db: Database) -> Result<Self, db::Error> {
        db.migrate(OWNER, MIGRATIONS).await?;
        Ok(Self::with(Some(db)))
    }

    /// Pictures kept in memory only, for tests.
    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self::with(None)
    }

    fn with(db: Option<Database>) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                cache: Mutex::default(),
                noted: Mutex::default(),
                files: Mutex::default(),
            }),
        }
    }

    /// Keeps `picture`, which someone added, before anyone refers to it.
    pub async fn add(&self, picture: Picture) -> Result<(), db::Error> {
        if let Some(db) = &self.inner.db {
            let kept = picture.clone();
            db.write(move |tx| store(tx, &[kept])).await?;
        }
        self.note_used(std::slice::from_ref(&picture.hash));
        self.inner.cache.lock().unwrap().put(picture);
        Ok(())
    }

    /// Keeps pictures a file brought, from code that can't wait, such as a
    /// format taking in a file changed outside Docs. They're in memory at
    /// once, and kept for good soon after.
    pub fn remember(&self, pictures: Pictures) {
        if pictures.is_empty() {
            return;
        }
        let pictures: Vec<Picture> = pictures.into_values().collect();
        {
            let mut cache = self.inner.cache.lock().unwrap();
            for picture in &pictures {
                cache.put(picture.clone());
            }
        }
        if let (Some(db), Ok(runtime)) = (&self.inner.db, tokio::runtime::Handle::try_current()) {
            let db = db.clone();
            runtime.spawn(async move {
                if let Err(err) = db.write(move |tx| store(tx, &pictures)).await {
                    tracing::warn!(%err, "couldn't keep a document's pictures");
                }
            });
        }
    }

    /// The pictures with `hashes` that are here, for saving a document.
    /// It waits on the database, so it suits code that can't await, and
    /// only reads what memory doesn't have.
    pub fn for_saving(&self, hashes: &[String]) -> Pictures {
        let mut found = Pictures::new();
        let mut missing = Vec::new();
        {
            let mut cache = self.inner.cache.lock().unwrap();
            for hash in hashes {
                match cache.get(hash) {
                    Some(picture) => {
                        found.insert(hash.clone(), picture);
                    }
                    None => missing.push(hash.clone()),
                }
            }
        }
        if !missing.is_empty()
            && let Some(db) = &self.inner.db
        {
            let wanted = missing.clone();
            let read = db.read_blocking(move |conn| -> rusqlite::Result<Vec<Picture>> {
                let mut statement =
                    conn.prepare_cached("SELECT contents FROM docs_images WHERE hash = ?1")?;
                let mut read = Vec::new();
                for hash in &wanted {
                    if let Some(Some(picture)) =
                        statement.query_row([hash], picture_from_row).optional()?
                    {
                        read.push(picture);
                    }
                }
                Ok(read)
            });
            match read {
                Ok(read) => {
                    let mut cache = self.inner.cache.lock().unwrap();
                    for picture in read {
                        cache.put(picture.clone());
                        found.insert(picture.hash.clone(), picture);
                    }
                }
                Err(err) => tracing::warn!(%err, "couldn't read a document's pictures"),
            }
        }
        for hash in hashes {
            if !found.contains_key(hash) {
                tracing::warn!(%hash, "a document's picture is missing, so its save leaves it out");
            }
        }
        self.note_used(hashes);
        found
    }

    /// The picture with `hash`, if it's here.
    pub async fn get(&self, hash: &str) -> Result<Option<Picture>, db::Error> {
        if let Some(picture) = self.inner.cache.lock().unwrap().get(hash) {
            return Ok(Some(picture));
        }
        let Some(db) = &self.inner.db else {
            return Ok(None);
        };
        let wanted = hash.to_owned();
        let picture = db
            .read(move |conn| {
                conn.query_row(
                    "SELECT contents FROM docs_images WHERE hash = ?1",
                    [wanted],
                    picture_from_row,
                )
                .optional()
                .map(Option::flatten)
            })
            .await?;
        if let Some(picture) = &picture {
            self.inner.cache.lock().unwrap().put(picture.clone());
        }
        Ok(picture)
    }

    /// Whether the pictures in the file with this hash are known to be here.
    pub fn knows_file(&self, file: &str) -> bool {
        self.inner.files.lock().unwrap().contains(file)
    }

    /// Makes sure the pictures in a document's file are here, before
    /// someone opens it. `file` is the file's hash, so each version is
    /// read once. The file is only read whole when a picture is missing.
    pub async fn take_in_file(&self, file: &str, contents: &[u8]) -> Result<(), db::Error> {
        if self.inner.files.lock().unwrap().contains(file) {
            return Ok(());
        }
        let hashes = hashes_written_in(contents);
        let mut missing = Vec::new();
        for hash in &hashes {
            if self.get(hash).await?.is_none() {
                missing.push(hash.clone());
            }
        }
        if !missing.is_empty() {
            let pictures = html::read_document(contents).pictures;
            let found: Vec<Picture> = pictures.into_values().collect();
            if let Some(db) = &self.inner.db {
                let kept = found.clone();
                db.write(move |tx| store(tx, &kept)).await?;
            }
            let mut cache = self.inner.cache.lock().unwrap();
            for picture in found {
                cache.put(picture);
            }
        }
        self.note_used(&hashes);
        self.inner.files.lock().unwrap().insert(file.to_owned());
        Ok(())
    }

    /// Notes that documents still use these pictures, at most once a day
    /// each, so they aren't left out.
    fn note_used(&self, hashes: &[String]) {
        let mut due = Vec::new();
        {
            let mut noted = self.inner.noted.lock().unwrap();
            for hash in hashes {
                let fresh = noted
                    .get(hash)
                    .is_some_and(|when| when.elapsed() < NOTE_USE_EVERY);
                if !fresh {
                    noted.insert(hash.clone(), Instant::now());
                    due.push(hash.clone());
                }
            }
        }
        if due.is_empty() {
            return;
        }
        if let (Some(db), Ok(runtime)) = (&self.inner.db, tokio::runtime::Handle::try_current()) {
            let db = db.clone();
            runtime.spawn(async move {
                let noted = db
                    .write(move |tx| -> rusqlite::Result<()> {
                        let mut statement = tx.prepare_cached(
                            "UPDATE docs_images SET used_at = ?2 WHERE hash = ?1",
                        )?;
                        let at = now();
                        for hash in &due {
                            statement.execute(rusqlite::params![hash, at])?;
                        }
                        Ok(())
                    })
                    .await;
                if let Err(err) = noted {
                    tracing::warn!(%err, "couldn't note which pictures documents use");
                }
            });
        }
    }

    /// Leaves out pictures no document has used for months. Their files
    /// still have them.
    pub async fn tidy(&self) -> Result<usize, db::Error> {
        let Some(db) = &self.inner.db else {
            return Ok(0);
        };
        let before = now() - UNUSED_FOR.as_secs() as i64;
        let removed = db
            .write(move |tx| tx.execute("DELETE FROM docs_images WHERE used_at < ?1", [before]))
            .await?;
        if removed > 0 {
            // A file read before may hold one of them.
            self.inner.files.lock().unwrap().clear();
        }
        Ok(removed)
    }
}

/// Keeps pictures in `docs_images`, each once.
fn store(tx: &rusqlite::Transaction, pictures: &[Picture]) -> rusqlite::Result<()> {
    let mut statement = tx.prepare_cached(
        "INSERT INTO docs_images (hash, content_type, contents, width, height, used_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (hash) DO UPDATE SET used_at = excluded.used_at",
    )?;
    let at = now();
    for picture in pictures {
        statement.execute(rusqlite::params![
            picture.hash,
            picture.content_type,
            &picture.contents[..],
            picture.width,
            picture.height,
            at
        ])?;
    }
    Ok(())
}

/// The picture hashes Docs wrote in a file, found quickly without reading
/// the page: each `data-hash` attribute.
fn hashes_written_in(contents: &[u8]) -> Vec<String> {
    const ATTRIBUTE: &[u8] = b"data-hash=\"";
    let mut hashes = Vec::new();
    let mut rest = contents;
    while let Some(at) = rest
        .windows(ATTRIBUTE.len())
        .position(|window| window == ATTRIBUTE)
    {
        rest = &rest[at + ATTRIBUTE.len()..];
        if let Some(hash) = rest
            .get(..64)
            .and_then(|hash| std::str::from_utf8(hash).ok())
            .filter(|hash| picture::is_hash(hash))
            && !hashes.iter().any(|known: &String| known == hash)
        {
            hashes.push(hash.to_owned());
        }
    }
    hashes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::picture::tests::png;

    #[test]
    fn finds_the_hashes_docs_wrote() {
        let a = picture::hash(b"a");
        let b = picture::hash(b"b");
        let file = format!(
            "<img src=\"data:...\" data-hash=\"{a}\"><img data-hash=\"{b}\"><img data-hash=\"{a}\"><img data-hash=\"nope\">"
        );
        assert_eq!(hashes_written_in(file.as_bytes()), vec![a, b]);
    }

    #[test]
    fn keeps_the_most_recently_used_in_memory() {
        let mut cache = Cache::default();
        let mut big = png(1, 1);
        big.resize(CACHE_SIZE / 3 + 1, 0);
        let first = Picture::read(big.clone()).unwrap();
        big[100] = 1;
        let second = Picture::read(big.clone()).unwrap();
        big[100] = 2;
        let third = Picture::read(big).unwrap();
        cache.put(first.clone());
        cache.put(second.clone());
        // The first is used again, so the second goes.
        std::thread::sleep(Duration::from_millis(2));
        assert!(cache.get(&first.hash).is_some());
        cache.put(third.clone());
        assert!(cache.get(&first.hash).is_some());
        assert!(cache.get(&second.hash).is_none());
        assert!(cache.get(&third.hash).is_some());
        assert!(cache.size <= CACHE_SIZE);
    }
}
