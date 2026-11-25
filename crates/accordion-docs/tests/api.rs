//! Docs' API, mounted the way the server mounts it, with two people.

use std::time::Duration;

use accordion_core::coedit::{CoEditing, Timing};
use accordion_core::db::Database;
use accordion_core::events::{Actor, Events, Via};
use accordion_core::identity::{AuthMethod, Client, Identity, NewUser};
use accordion_core::live::Live;
use accordion_core::storage::{ShareLevel, Storage};
use accordion_core::takeout::{self, Imports, Takeout};
use accordion_core::texts::{self, Texts};
use accordion_core::thumbnails::{Thumbnails, Tools};
use accordion_core::transcripts::Transcripts;
use accordion_docs::Docs;
use accordion_docs::html::{self, MARKER};
use accordion_docs::model::{Block, Image, Inline, Marks, Para};
use accordion_docs::picture::{self, Picture};
use accordion_module::{Module, Services, Stopping};
use accordion_types::{ItemId, Role, UserId};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::sync::watch;
use tower::ServiceExt;

/// Saves quickly, so tests see edits reach their files.
const QUICK: Timing = Timing {
    save_after: Duration::from_millis(200),
    save_at_least_every: Duration::from_secs(2),
    check_access_after: Duration::from_millis(50),
};

struct Server {
    _dir: tempfile::TempDir,
    _stop: watch::Sender<bool>,
    router: Router,
    storage: Storage,
    thumbnails: Thumbnails,
    alex: Person,
    sam: Person,
}

struct Person {
    id: UserId,
    bearer: String,
}

async fn start() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("accordion.db")).unwrap();
    let identity = Identity::new(db.clone()).await.unwrap();
    let storage = Storage::open(db.clone(), identity.clone(), &dir.path().join("storage"))
        .await
        .unwrap();
    // Pictures people add are shrunk in the sandbox. Docs never reads text
    // with programs or transcribes, so those point nowhere.
    let thumbnails = Thumbnails::open(
        db.clone(),
        storage.clone(),
        dir.path(),
        Tools::installed(env!("CARGO_BIN_EXE_accordion-docs-sandbox")),
    )
    .await
    .unwrap();
    let texts = Texts::open(
        db.clone(),
        storage.clone(),
        thumbnails.clone(),
        dir.path(),
        texts::Tools::installed("/nonexistent"),
    )
    .await
    .unwrap();
    let events = Events::open(db.clone()).await.unwrap();
    // Docs imports nothing from Google Takeout.
    let takeout = Imports::open(
        db.clone(),
        storage.clone(),
        events.clone(),
        Takeout::new(takeout::Tools::installed("/nonexistent"), dir.path()),
    )
    .await
    .unwrap();
    let services = Services {
        db: db.clone(),
        coediting: CoEditing::open(db.clone(), storage.clone(), identity.clone(), QUICK)
            .await
            .unwrap(),
        identity: identity.clone(),
        storage: storage.clone(),
        events,
        thumbnails: thumbnails.clone(),
        texts,
        transcripts: Transcripts::canned(db.clone(), storage.clone(), dir.path(), |_| None)
            .await
            .unwrap(),
        live: Live::new(db, &identity),
        link_origin: watch::channel(None).1,
        takeout,
    };
    let (stop, stopping) = watch::channel(false);
    let started = Docs.start(services, Stopping::new(stopping)).await.unwrap();
    Server {
        router: Router::new().nest("/api/docs", started.routes.api.unwrap()),
        alex: person(&identity, "alex", "Alex Kim", Role::Owner).await,
        sam: person(&identity, "sam", "Sam Kim", Role::User).await,
        storage,
        thumbnails,
        _dir: dir,
        _stop: stop,
    }
}

async fn person(identity: &Identity, username: &str, name: &str, role: Role) -> Person {
    let user = identity
        .create_user(NewUser {
            username: username.into(),
            display_name: name.into(),
            role,
            password: "correct horse".into(),
        })
        .await
        .unwrap();
    let (token, _) = identity
        .create_session(user.id, Client::App, "Laptop", AuthMethod::Password)
        .await
        .unwrap();
    Person {
        id: user.id,
        bearer: format!("Bearer {}", token.as_str()),
    }
}

impl Server {
    async fn api(
        &self,
        person: &Person,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(format!("/api/docs{path}"))
            .header("authorization", &person.bearer);
        let body = match body {
            Some(json) => {
                request = request.header("content-type", "application/json");
                Body::from(json.to_string())
            }
            None => Body::empty(),
        };
        let response = self
            .router
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        (status, value)
    }

    /// Storage acting as `person`, the way Drive, WebDAV, or sync would.
    fn as_person(&self, person: &Person) -> Storage {
        self.storage
            .acting_as(Actor::person(person.id, Via::WebDav))
    }

    async fn new_document(&self, person: &Person) -> String {
        let (status, document) = self.api(person, "POST", "/documents", None).await;
        assert_eq!(status, StatusCode::CREATED, "{document}");
        document["id"].as_str().unwrap().to_owned()
    }

    async fn file_text(&self, id: &str) -> String {
        let id: ItemId = id.parse().unwrap();
        let mut file = self.storage.open_file(id).await.unwrap();
        let mut text = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut file, &mut text)
            .await
            .unwrap();
        text
    }

    async fn names(&self, person: &Person) -> Vec<String> {
        let (status, list) = self.api(person, "GET", "/documents", None).await;
        assert_eq!(status, StatusCode::OK, "{list}");
        let mut names: Vec<String> = list["documents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|document| document["name"].as_str().unwrap().to_owned())
            .collect();
        names.sort();
        names
    }
}

impl Server {
    /// Sends `body` as raw bytes, and returns the status, the headers, and
    /// the bytes that came back.
    async fn send(
        &self,
        person: &Person,
        method: &str,
        path: &str,
        body: Vec<u8>,
    ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(format!("/api/docs{path}"))
                    .header("authorization", &person.bearer)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, bytes.to_vec())
    }
}

/// A photo, made with libvips outside the sandbox.
fn photo(dir: &std::path::Path, width: u32, height: u32) -> Vec<u8> {
    let path = dir.join("photo.jpg");
    let made = std::process::Command::new("vips")
        .args(["xyz", path.to_str().unwrap()])
        .args([width.to_string(), height.to_string()])
        .status()
        .unwrap();
    assert!(made.success());
    std::fs::read(path).unwrap()
}

#[tokio::test]
async fn adds_pictures_to_documents() {
    let server = start().await;
    let id = server.new_document(&server.alex).await;
    let dir = tempfile::tempdir().unwrap();

    // A large photo is shrunk to 2,560 pixels on its longer side.
    let (status, _, added) = server
        .send(
            &server.alex,
            "POST",
            &format!("/documents/{id}/images"),
            photo(dir.path(), 4000, 3000),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let added: Value = serde_json::from_slice(&added).unwrap();
    assert_eq!(
        (added["width"].as_u64(), added["height"].as_u64()),
        (Some(2560), Some(1920))
    );
    let hash = added["hash"].as_str().unwrap().to_owned();

    let (status, headers, bytes) = server
        .send(
            &server.alex,
            "GET",
            &format!("/documents/{id}/images/{hash}"),
            Vec::new(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "image/jpeg");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert!(
        headers["cache-control"]
            .to_str()
            .unwrap()
            .contains("immutable")
    );
    assert_eq!(picture::hash(&bytes), hash);

    // Only people who can open the document see its pictures.
    let (status, _, _) = server
        .send(
            &server.sam,
            "GET",
            &format!("/documents/{id}/images/{hash}"),
            Vec::new(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = server
        .send(
            &server.sam,
            "POST",
            &format!("/documents/{id}/images"),
            photo(dir.path(), 10, 10),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Only pictures, not pages or drawings that could run scripts.
    let (status, _, said) = server
        .send(
            &server.alex,
            "POST",
            &format!("/documents/{id}/images"),
            b"<svg onload=steal()>".to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&said).contains("JPEG, PNG, GIF, and WebP"));

    // A picture only the file has, such as one in a file from another
    // server, is found there. Word files carry it.
    let picture = Picture::read(photo(dir.path(), 300, 200)).unwrap();
    let image = Block::Image(Image {
        hash: picture.hash.clone(),
        alt: "A test pattern".into(),
        width: None,
    });
    let file = html::page(
        &[paragraph("Look"), image],
        &accordion_docs::model::Parts::default(),
        None,
        None,
        &[(picture.hash.clone(), picture.clone())]
            .into_iter()
            .collect(),
    );
    let (_, document) = server
        .api(&server.alex, "GET", &format!("/documents/{id}"), None)
        .await;
    let top = server
        .storage
        .place_folder(server.alex.id, "docs")
        .await
        .unwrap();
    server
        .as_person(&server.alex)
        .write_file(top, document["name"].as_str().unwrap(), file.as_bytes())
        .await
        .unwrap();
    let (status, _, bytes) = server
        .send(
            &server.alex,
            "GET",
            &format!("/documents/{id}/images/{}", picture.hash),
            Vec::new(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, &picture.contents[..]);
    let (status, _, word) = server
        .send(
            &server.alex,
            "GET",
            &format!("/documents/{id}/download?as=docx"),
            Vec::new(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let word = String::from_utf8_lossy(&word);
    assert!(word.contains("word/media/picture0.jpg"));
}

fn paragraph(text: &str) -> Block {
    Block::Paragraph(
        Para::default(),
        vec![Inline::Text(text.into(), Marks::default())],
    )
}

#[tokio::test]
async fn makes_documents_in_the_docs_place() {
    let server = start().await;
    let first = server.new_document(&server.alex).await;
    server.new_document(&server.alex).await;
    assert!(server.file_text(&first).await.starts_with(MARKER));

    // Other HTML files in the Docs place are web pages, not documents, and
    // documents elsewhere in Drive open from Drive.
    let top = server
        .storage
        .place_folder(server.alex.id, "docs")
        .await
        .unwrap();
    let alex = server.as_person(&server.alex);
    alex.create_file(top, "Page.html", b"<!DOCTYPE html><p>Hi</p>".as_slice())
        .await
        .unwrap();
    let my_drive = server.storage.top_folder(server.alex.id).await.unwrap();
    let elsewhere = alex
        .create_file(
            my_drive,
            "Elsewhere.html",
            html::page(
                &[paragraph("Hi")],
                &accordion_docs::model::Parts::default(),
                None,
                None,
                &Default::default(),
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    assert_eq!(
        server.names(&server.alex).await,
        ["Untitled document (2).html", "Untitled document.html"]
    );
    let (status, opened) = server
        .api(
            &server.alex,
            "GET",
            &format!("/documents/{}", elsewhere.id),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{opened}");
    assert_eq!(opened["access"], "owner");

    let (_, list) = server.api(&server.alex, "GET", "/documents", None).await;
    assert_eq!(list["top"], json!(top));
}

#[tokio::test]
async fn downloads_a_document_in_each_format() {
    let server = start().await;
    let (status, made) = server
        .api(
            &server.alex,
            "POST",
            "/documents",
            Some(json!({"page": "a4"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let id = made["id"].as_str().unwrap().to_owned();
    assert!(server.file_text(&id).await.contains("data-page=\"a4\""));

    // Someone changes the file outside Docs, with a script and the
    // co-editing state in it. Only the file itself keeps them.
    let file = server.file_text(&id).await;
    let changed = file.replace(
        "<p></p>",
        "<p>Hello</p><script>steal()</script><img src=x onerror=steal()>",
    );
    let changed = changed.replace(
        "</main>",
        "</main>\n<script type=\"application/x-accordion-docs\">{\"version\":1,\"state\":\"AAA=\"}</script>",
    );
    let top = server
        .storage
        .place_folder(server.alex.id, "docs")
        .await
        .unwrap();
    server
        .as_person(&server.alex)
        .write_file(top, made["name"].as_str().unwrap(), changed.as_bytes())
        .await
        .unwrap();

    // Downloading gives the file itself, data block and all, to keep.
    let response = server
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/docs/documents/{id}/download?as=html"))
                .header("authorization", &server.alex.bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-disposition"],
        "attachment; filename=\"Untitled document.html\"; filename*=UTF-8''Untitled%20document.html"
    );
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(bytes, changed.as_bytes());

    // Or as Markdown, Word, or OpenDocument, named after the document.
    for (format, kind, start) in [
        ("md", "text/markdown; charset=utf-8", &b"Hello\n"[..]),
        (
            "docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            &b"PK"[..],
        ),
        ("odt", "application/vnd.oasis.opendocument.text", &b"PK"[..]),
    ] {
        let response = server
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/docs/documents/{id}/download?as={format}"))
                    .header("authorization", &server.alex.bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{format}");
        assert_eq!(response.headers()["content-type"], kind);
        assert_eq!(
            response.headers()["content-disposition"],
            format!(
                "attachment; filename=\"Untitled document.{format}\"; filename*=UTF-8''Untitled%20document.{format}"
            )
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(bytes.starts_with(start), "{format}: {bytes:?}");
        assert!(!bytes.windows(5).any(|w| w == b"steal"), "{format}");
    }
    let (status, _) = server
        .api(
            &server.alex,
            "GET",
            &format!("/documents/{id}/download?as=exe"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Only people who can open the document can download it.
    let (status, _) = server
        .api(
            &server.sam,
            "GET",
            &format!("/documents/{id}/download?as=docx"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn lists_each_documents_page_thumbnail() {
    let server = start().await;
    let id = server.new_document(&server.alex).await;
    let (_, list) = server.api(&server.alex, "GET", "/documents", None).await;
    assert_eq!(list["documents"][0]["thumbnail"], json!(null));

    let file = server
        .storage
        .item(id.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    let accordion_core::storage::ItemKind::File { blob, .. } = file.kind else {
        panic!("a document is a file");
    };
    assert!(server.thumbnails.make_now(blob.hash).await.unwrap());
    let (_, list) = server.api(&server.alex, "GET", "/documents", None).await;
    let listed = &list["documents"][0];
    assert_eq!(listed["thumbnail"], "page");
    assert_eq!(listed["sha256"], blob.hash.to_hex());
    assert!(listed["thumbnail_version"].is_string(), "{listed}");
}

#[tokio::test]
async fn people_reach_only_documents_shared_with_them() {
    let server = start().await;
    let id = server.new_document(&server.alex).await;
    let (status, _) = server
        .api(&server.sam, "GET", &format!("/documents/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(server.names(&server.sam).await.is_empty());

    server
        .as_person(&server.alex)
        .share(
            id.parse().unwrap(),
            server.sam.id,
            ShareLevel::Viewer,
            server.alex.id,
        )
        .await
        .unwrap();
    let (status, opened) = server
        .api(&server.sam, "GET", &format!("/documents/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{opened}");
    assert_eq!(opened["access"], "viewer");
    assert_eq!(opened["owner"], "Alex Kim");
    let (_, list) = server.api(&server.sam, "GET", "/documents", None).await;
    assert_eq!(list["documents"][0]["owner"], "Alex Kim");
    assert_eq!(list["documents"][0]["access"], "viewer");

    // A document in the trash is gone for everyone.
    server
        .as_person(&server.alex)
        .delete(id.parse().unwrap())
        .await
        .unwrap();
    let (status, _) = server
        .api(&server.alex, "GET", &format!("/documents/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn copies_a_document_beside_it_or_into_the_viewers_docs() {
    let server = start().await;
    let id = server.new_document(&server.alex).await;
    let copy = |name: &str| json!({ "name": name });
    let (status, made) = server
        .api(
            &server.alex,
            "POST",
            &format!("/documents/{id}/copy"),
            Some(copy("Copy of Plans")),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    assert_eq!(made["name"], "Copy of Plans.html");
    assert_eq!(made["access"], "owner");
    assert_eq!(
        server.file_text(made["id"].as_str().unwrap()).await,
        server.file_text(&id).await
    );
    // A name that's taken gets a number.
    let (_, again) = server
        .api(
            &server.alex,
            "POST",
            &format!("/documents/{id}/copy"),
            Some(copy("Copy of Plans.html")),
        )
        .await;
    assert_eq!(again["name"], "Copy of Plans (2).html");
    let (status, _) = server
        .api(
            &server.alex,
            "POST",
            &format!("/documents/{id}/copy"),
            Some(copy("  ")),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Nobody else can copy it, until it's shared with them. A viewer's copy
    // is theirs, in their own Docs.
    let (status, _) = server
        .api(
            &server.sam,
            "POST",
            &format!("/documents/{id}/copy"),
            Some(copy("Mine")),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    server
        .as_person(&server.alex)
        .share(
            id.parse().unwrap(),
            server.sam.id,
            ShareLevel::Viewer,
            server.alex.id,
        )
        .await
        .unwrap();
    let (status, mine) = server
        .api(
            &server.sam,
            "POST",
            &format!("/documents/{id}/copy"),
            Some(copy("Mine")),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{mine}");
    assert_eq!(mine["access"], "owner");
    assert_eq!(mine["owner"], Value::Null);
    assert_eq!(
        server.names(&server.sam).await,
        ["Mine.html", "Untitled document.html"]
    );
}

/// A new thread on `id` from `person`, with made-up positions.
fn new_thread(text: &str) -> Value {
    json!({ "start": "AQID", "end": "BAUG", "quote": "  the\n plan ", "text": text })
}

#[tokio::test]
async fn comments_are_kept_with_the_document_and_in_its_file() {
    let server = start().await;
    let id = server.new_document(&server.alex).await;
    let comments = format!("/documents/{id}/comments");
    let (status, none) = server.api(&server.alex, "GET", &comments, None).await;
    assert_eq!(status, StatusCode::OK, "{none}");
    assert_eq!(none, json!({ "can_comment": true, "threads": [] }));

    let (status, view) = server
        .api(
            &server.alex,
            "POST",
            &comments,
            Some(new_thread(" Is this </script> right? ")),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{view}");
    let thread = &view["threads"][0];
    let thread_id = thread["id"].as_str().unwrap().to_owned();
    assert_eq!(thread["quote"], "the plan");
    assert_eq!(thread["resolved"], Value::Null);
    assert_eq!(thread["comments"][0]["text"], "Is this </script> right?");
    assert_eq!(thread["comments"][0]["name"], "Alex Kim");
    assert_eq!(thread["comments"][0]["mine"], true);

    // The file carries the thread, and still reads as before.
    let file = server.file_text(&id).await;
    let kept = html::comments_in(file.as_bytes());
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].comments[0].text, "Is this </script> right?");
    assert_eq!(kept[0].comments[0].author, Some(server.alex.id));
    assert_eq!(
        html::read(file.as_bytes()),
        [Block::Paragraph(Para::default(), Vec::new())]
    );

    // Viewers see comments but can't add them.
    let alex_storage = server.as_person(&server.alex);
    alex_storage
        .share(
            id.parse().unwrap(),
            server.sam.id,
            ShareLevel::Viewer,
            server.alex.id,
        )
        .await
        .unwrap();
    let (status, seen) = server.api(&server.sam, "GET", &comments, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seen["can_comment"], false);
    assert_eq!(seen["threads"][0]["comments"][0]["mine"], false);
    let (status, _) = server
        .api(&server.sam, "POST", &comments, Some(new_thread("Mine")))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Commenters comment, reply, and resolve, but can't change the
    // document or delete someone else's comment.
    alex_storage
        .share(
            id.parse().unwrap(),
            server.sam.id,
            ShareLevel::Commenter,
            server.alex.id,
        )
        .await
        .unwrap();
    let (status, opened) = server
        .api(&server.sam, "GET", &format!("/documents/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(opened["access"], "commenter");
    let (status, view) = server
        .api(&server.sam, "POST", &comments, Some(new_thread("From Sam")))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{view}");
    assert_eq!(view["can_comment"], true);
    let sams = view["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|thread| thread["comments"][0]["text"] == "From Sam")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, view) = server
        .api(
            &server.sam,
            "POST",
            &format!("{comments}/{thread_id}"),
            Some(json!({ "text": "Seen" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{view}");
    let alexs = view["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|thread| thread["id"] == thread_id.as_str())
        .unwrap();
    assert_eq!(alexs["can_delete"], false);
    let (status, _) = server
        .api(
            &server.sam,
            "DELETE",
            &format!("{comments}/{thread_id}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, view) = server
        .api(
            &server.sam,
            "PATCH",
            &format!("{comments}/{sams}"),
            Some(json!({ "resolved": true })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let (status, view) = server
        .api(&server.sam, "DELETE", &format!("{comments}/{sams}"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    // Take Sam's reply away again, so what follows starts as it did.
    let reply = view["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|thread| thread["id"] == thread_id.as_str())
        .unwrap()["comments"][1]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, _) = server
        .api(
            &server.sam,
            "DELETE",
            &format!("{comments}/{thread_id}/{reply}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = server
        .send(
            &server.sam,
            "POST",
            &format!("/documents/{id}/images"),
            b"not a picture".to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Editors reply, and change only their own comments.
    alex_storage
        .share(
            id.parse().unwrap(),
            server.sam.id,
            ShareLevel::Editor,
            server.alex.id,
        )
        .await
        .unwrap();
    let thread_path = format!("{comments}/{thread_id}");
    let (status, view) = server
        .api(
            &server.sam,
            "POST",
            &thread_path,
            Some(json!({ "text": "Yes" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{view}");
    let first = view["threads"][0]["comments"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let reply = view["threads"][0]["comments"][1]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(view["threads"][0]["comments"][1]["name"], "Sam Kim");
    let (status, _) = server
        .api(
            &server.sam,
            "PATCH",
            &format!("{thread_path}/{first}"),
            Some(json!({ "text": "No" })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, view) = server
        .api(
            &server.sam,
            "PATCH",
            &format!("{thread_path}/{reply}"),
            Some(json!({ "text": "Yes, it is" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["threads"][0]["comments"][1]["text"], "Yes, it is");
    assert!(view["threads"][0]["comments"][1]["edited"].is_string());

    // Resolving, and a reply opens the thread again.
    let (_, view) = server
        .api(
            &server.alex,
            "PATCH",
            &thread_path,
            Some(json!({ "resolved": true })),
        )
        .await;
    assert_eq!(view["threads"][0]["resolved"]["name"], "Alex Kim");
    let (_, view) = server
        .api(
            &server.sam,
            "POST",
            &thread_path,
            Some(json!({ "text": "One more thing" })),
        )
        .await;
    assert_eq!(view["threads"][0]["resolved"], Value::Null);
    assert_eq!(view["threads"][0]["comments"].as_array().unwrap().len(), 3);

    // What isn't a comment is turned away.
    for (body, expected) in [
        (new_thread("   "), StatusCode::BAD_REQUEST),
        (new_thread(&"x".repeat(4001)), StatusCode::BAD_REQUEST),
        (
            json!({ "start": "not base64!", "end": "AQID", "text": "Hi" }),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, _) = server
            .api(&server.alex, "POST", &comments, Some(body))
            .await;
        assert_eq!(status, expected);
    }
    let (status, _) = server
        .api(
            &server.alex,
            "POST",
            &format!("{comments}/nothing"),
            Some(json!({ "text": "Hi" })),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Restoring an older version of the file keeps the comments as they
    // are, as in Google Docs.
    let item = server
        .storage
        .item(id.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    let accordion_core::storage::ItemKind::File { blob, .. } = item.kind else {
        panic!("a folder");
    };
    let older = html::with_comments(file.as_bytes(), &[]).unwrap();
    alex_storage
        .write_version(item.id, blob.hash, older.as_slice())
        .await
        .unwrap();
    let (_, view) = server.api(&server.alex, "GET", &comments, None).await;
    assert_eq!(view["threads"][0]["comments"].as_array().unwrap().len(), 3);

    // A copy leaves the comments out, unless asked.
    let (_, plain) = server
        .api(
            &server.alex,
            "POST",
            &format!("/documents/{id}/copy"),
            Some(json!({ "name": "Plain" })),
        )
        .await;
    let plain = plain["id"].as_str().unwrap().to_owned();
    assert!(html::comments_in(server.file_text(&plain).await.as_bytes()).is_empty());
    let (_, view) = server
        .api(
            &server.alex,
            "GET",
            &format!("/documents/{plain}/comments"),
            None,
        )
        .await;
    assert_eq!(view["threads"], json!([]));
    // Comments written after the restore are in the file again.
    server
        .api(
            &server.alex,
            "PATCH",
            &thread_path,
            Some(json!({ "resolved": true })),
        )
        .await;
    let (_, kept) = server
        .api(
            &server.alex,
            "POST",
            &format!("/documents/{id}/copy"),
            Some(json!({ "name": "With comments", "comments": true })),
        )
        .await;
    let (_, view) = server
        .api(
            &server.alex,
            "GET",
            &format!("/documents/{}/comments", kept["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(view["threads"][0]["comments"].as_array().unwrap().len(), 3);
    assert_eq!(view["threads"][0]["resolved"]["name"], "Alex Kim");

    // Deleting a reply, then the first comment, which takes the thread.
    let (status, view) = server
        .api(
            &server.alex,
            "DELETE",
            &format!("{thread_path}/{reply}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["threads"][0]["comments"].as_array().unwrap().len(), 2);
    let (status, view) = server
        .api(
            &server.alex,
            "DELETE",
            &format!("{thread_path}/{first}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["threads"], json!([]));
    assert!(html::comments_in(server.file_text(&id).await.as_bytes()).is_empty());
}

#[tokio::test]
async fn a_file_brings_its_comments_with_it() {
    let server = start().await;
    let top = server
        .storage
        .place_folder(server.alex.id, "docs")
        .await
        .unwrap();
    let page = html::page(
        &[paragraph("Plan")],
        &Default::default(),
        None,
        None,
        &Default::default(),
    );
    let thread: accordion_docs::comments::Thread = serde_json::from_value(json!({
        "id": "t1",
        "start": "AQID",
        "end": "BAUG",
        "quote": "Plan",
        "comments": [{ "id": "c1", "name": "Dana Elsewhere", "text": "From another server", "at": "2026-11-01T09:00:00Z" }],
    }))
    .unwrap();
    let file = html::with_comments(page.as_bytes(), &[thread]).unwrap();
    let item = server
        .as_person(&server.alex)
        .create_file(top, "From elsewhere.html", file.as_slice())
        .await
        .unwrap();
    let (status, view) = server
        .api(
            &server.alex,
            "GET",
            &format!("/documents/{}/comments", item.id),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["threads"][0]["comments"][0]["name"], "Dana Elsewhere");
    assert_eq!(view["threads"][0]["comments"][0]["mine"], false);
    assert_eq!(view["threads"][0]["can_delete"], true);
}

#[tokio::test]
async fn new_documents_go_only_in_the_persons_docs_place() {
    let server = start().await;
    let my_drive = server.storage.top_folder(server.alex.id).await.unwrap();
    let (status, _) = server
        .api(
            &server.alex,
            "POST",
            "/documents",
            Some(json!({"folder": my_drive})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let top = server
        .storage
        .place_folder(server.alex.id, "docs")
        .await
        .unwrap();
    let folder = server
        .as_person(&server.alex)
        .create_folder(top, "Work")
        .await
        .unwrap();
    let (status, created) = server
        .api(
            &server.alex,
            "POST",
            "/documents",
            Some(json!({"folder": folder.id})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["folder"], json!(folder.id));
}

mod live {
    use std::time::Duration;

    use accordion_docs::yjs;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::{self, Message};
    use yrs::encoding::read::Cursor;
    use yrs::sync::{Message as YMessage, MessageReader, SyncMessage};
    use yrs::updates::decoder::{Decode, DecoderV1};
    use yrs::updates::encoder::Encode;
    use yrs::{Doc, OffsetKind, Options, ReadTxn, StateVector, Transact, Update, XmlFragment};

    use super::*;

    type Socket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    async fn serve(server: &Server) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = server.router.clone();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        address
    }

    async fn connect(
        address: std::net::SocketAddr,
        person: &Person,
        id: &str,
    ) -> Result<Socket, tungstenite::Error> {
        let mut request = format!("ws://{address}/api/docs/documents/{id}/live")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("authorization", person.bearer.parse().unwrap());
        tokio_tungstenite::connect_async(request)
            .await
            .map(|(socket, _)| socket)
    }

    /// A browser's copy of the document.
    struct Browser {
        doc: Doc,
        socket: Socket,
    }

    impl Browser {
        async fn open(address: std::net::SocketAddr, person: &Person, id: &str) -> Self {
            let socket = connect(address, person, id).await.unwrap();
            let doc = Doc::with_options(Options {
                offset_kind: OffsetKind::Utf16,
                ..Options::default()
            });
            let mut browser = Self { doc, socket };
            let step1 = YMessage::Sync(SyncMessage::SyncStep1(StateVector::default()));
            browser.send(step1).await;
            browser.take_in().await;
            browser
        }

        async fn send(&mut self, message: YMessage) {
            self.socket
                .send(Message::Binary(message.encode_v1().into()))
                .await
                .unwrap();
        }

        /// Applies what the server sends for a moment.
        async fn take_in(&mut self) {
            while let Ok(Some(Ok(message))) =
                tokio::time::timeout(Duration::from_millis(400), self.socket.next()).await
            {
                let Message::Binary(bytes) = message else {
                    continue;
                };
                let mut decoder = DecoderV1::new(Cursor::new(&bytes));
                for message in MessageReader::new(&mut decoder) {
                    if let YMessage::Sync(
                        SyncMessage::SyncStep2(update) | SyncMessage::Update(update),
                    ) = message.unwrap()
                    {
                        let update = Update::decode_v1(&update).unwrap();
                        self.doc.transact_mut().apply_update(update).unwrap();
                    }
                }
            }
        }

        fn blocks(&self) -> Vec<Block> {
            let fragment = self.doc.get_or_insert_xml_fragment(yjs::FRAGMENT);
            yjs::read(&self.doc.transact(), &fragment)
        }

        async fn wait_for(&mut self, expected: &[Block]) {
            for _ in 0..25 {
                if self.blocks() == expected {
                    return;
                }
                self.take_in().await;
            }
            assert_eq!(self.blocks(), expected);
        }

        /// Adds a paragraph at the end, as the editor would.
        async fn add_paragraph(&mut self, text: &str) {
            let fragment = self.doc.get_or_insert_xml_fragment(yjs::FRAGMENT);
            let before = self.doc.transact().state_vector();
            {
                let mut txn = self.doc.transact_mut();
                let end = fragment.len(&txn);
                yjs::insert_blocks(&mut txn, &fragment, end, &[paragraph(text)]);
            }
            let update = self.doc.transact().encode_diff_v1(&before);
            self.send(YMessage::Sync(SyncMessage::Update(update))).await;
        }
    }

    async fn file_blocks(server: &Server, id: &str) -> Vec<Block> {
        html::read(server.file_text(id).await.as_bytes())
    }

    #[tokio::test]
    async fn documents_are_edited_together_and_reach_the_file() {
        let server = start().await;
        let address = serve(&server).await;
        let id = server.new_document(&server.alex).await;

        // Nobody reaches a document that isn't shared with them.
        assert!(connect(address, &server.sam, &id).await.is_err());
        server
            .as_person(&server.alex)
            .share(
                id.parse().unwrap(),
                server.sam.id,
                ShareLevel::Editor,
                server.alex.id,
            )
            .await
            .unwrap();

        let mut alex = Browser::open(address, &server.alex, &id).await;
        assert_eq!(
            alex.blocks(),
            [Block::Paragraph(Para::default(), Vec::new())]
        );
        let mut sam = Browser::open(address, &server.sam, &id).await;
        alex.add_paragraph("From Alex").await;
        sam.add_paragraph("From Sam").await;
        let both = [
            Block::Paragraph(Para::default(), Vec::new()),
            paragraph("From Alex"),
            paragraph("From Sam"),
        ];
        let other_order = [
            Block::Paragraph(Para::default(), Vec::new()),
            paragraph("From Sam"),
            paragraph("From Alex"),
        ];
        alex.take_in().await;
        sam.take_in().await;
        assert_eq!(alex.blocks(), sam.blocks());
        assert!(alex.blocks() == both || alex.blocks() == other_order);

        tokio::time::sleep(Duration::from_millis(600)).await;
        let text = server.file_text(&id).await;
        assert!(text.starts_with(MARKER));
        assert_eq!(html::read(text.as_bytes()), alex.blocks());
        assert!(html::state_in(text.as_bytes()).is_some());
    }

    #[tokio::test]
    async fn a_document_started_in_the_browser_keeps_its_first_changes() {
        use base64::Engine as _;
        let server = start().await;
        let address = serve(&server).await;

        // The web app's blank document has no file until someone types.
        let draft = Doc::with_options(Options {
            offset_kind: OffsetKind::Utf16,
            ..Options::default()
        });
        {
            let fragment = draft.get_or_insert_xml_fragment(yjs::FRAGMENT);
            let mut txn = draft.transact_mut();
            yjs::insert_blocks(&mut txn, &fragment, 0, &[paragraph("First words")]);
        }
        let state = draft
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        let state = base64::engine::general_purpose::STANDARD.encode(state);
        let (status, document) = server
            .api(
                &server.alex,
                "POST",
                "/documents",
                Some(json!({ "page": "a4", "state": state })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{document}");
        let id = document["id"].as_str().unwrap().to_owned();
        assert_eq!(file_blocks(&server, &id).await, [paragraph("First words")]);

        // The browser joins with what it has, and nothing comes in twice.
        let socket = connect(address, &server.alex, &id).await.unwrap();
        let mut alex = Browser { doc: draft, socket };
        let step1 = YMessage::Sync(SyncMessage::SyncStep1(alex.doc.transact().state_vector()));
        alex.send(step1).await;
        alex.take_in().await;
        assert_eq!(alex.blocks(), [paragraph("First words")]);
        alex.add_paragraph("More").await;
        drop(alex);
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(
            file_blocks(&server, &id).await,
            [paragraph("First words"), paragraph("More")]
        );

        let (status, _) = server
            .api(
                &server.alex,
                "POST",
                "/documents",
                Some(json!({ "state": "bm90IGEgc3RhdGU=" })),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn comments_on_an_open_document_stay_in_its_file() {
        let server = start().await;
        let address = serve(&server).await;
        let id = server.new_document(&server.alex).await;
        let mut alex = Browser::open(address, &server.alex, &id).await;
        // A comment right after typing, before the new document's first
        // save, keeps the typing.
        alex.add_paragraph("Typed").await;
        tokio::time::sleep(Duration::from_millis(20)).await;

        let (status, view) = server
            .api(
                &server.alex,
                "POST",
                &format!("/documents/{id}/comments"),
                Some(super::new_thread("Check this")),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{view}");
        let file = server.file_text(&id).await;
        assert_eq!(html::comments_in(file.as_bytes()).len(), 1);
        assert_eq!(html::read(file.as_bytes()), alex.blocks());

        // The document's own saves keep the comments, and nothing the
        // editor had was lost to the comment's write.
        alex.take_in().await;
        alex.add_paragraph("More").await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        let file = server.file_text(&id).await;
        assert_eq!(html::comments_in(file.as_bytes()).len(), 1);
        assert_eq!(
            html::read(file.as_bytes()),
            [
                Block::Paragraph(Para::default(), Vec::new()),
                paragraph("Typed"),
                paragraph("More"),
            ]
        );
        assert_eq!(alex.blocks(), html::read(file.as_bytes()));
    }

    #[tokio::test]
    async fn a_change_to_the_file_reaches_everyone_editing() {
        let server = start().await;
        let address = serve(&server).await;
        let id = server.new_document(&server.alex).await;
        let mut alex = Browser::open(address, &server.alex, &id).await;
        alex.add_paragraph("Typed").await;
        tokio::time::sleep(Duration::from_millis(600)).await;

        // Someone saves an edited copy, such as from a synced computer.
        let saved = server.file_text(&id).await;
        let edited = saved.replace("<p>Typed</p>", "<p>Typed</p>\n<h2>Added elsewhere</h2>");
        let item = server
            .storage
            .item(id.parse().unwrap())
            .await
            .unwrap()
            .unwrap();
        let accordion_core::storage::ItemKind::File { blob, .. } = item.kind else {
            panic!("a folder");
        };
        server
            .as_person(&server.alex)
            .write_version(item.id, blob.hash, edited.as_bytes())
            .await
            .unwrap();
        alex.wait_for(&[
            Block::Paragraph(Para::default(), Vec::new()),
            paragraph("Typed"),
            Block::Heading(
                2,
                Para::default(),
                vec![Inline::Text("Added elsewhere".into(), Marks::default())],
            ),
        ])
        .await;
        assert_eq!(file_blocks(&server, &id).await, alex.blocks());
    }

    #[tokio::test]
    async fn viewers_and_commenters_watch_but_their_edits_dont_land() {
        let server = start().await;
        let address = serve(&server).await;
        for level in [ShareLevel::Viewer, ShareLevel::Commenter] {
            let id = server.new_document(&server.alex).await;
            server
                .as_person(&server.alex)
                .share(id.parse().unwrap(), server.sam.id, level, server.alex.id)
                .await
                .unwrap();
            let mut sam = Browser::open(address, &server.sam, &id).await;
            sam.add_paragraph("Not allowed").await;
            tokio::time::sleep(Duration::from_millis(600)).await;
            assert_eq!(
                file_blocks(&server, &id).await,
                [Block::Paragraph(Para::default(), Vec::new())],
                "{level:?}"
            );
        }
    }
}

#[tokio::test]
async fn opens_word_files_and_pages_with_docs() {
    let server = start().await;
    let dir = tempfile::tempdir().unwrap();
    let my_drive = server.storage.top_folder(server.alex.id).await.unwrap();
    let alex = server.as_person(&server.alex);

    // A Word file with a large photo, as Docs' own download writes one.
    let photo = Picture::read(photo(dir.path(), 3000, 2000)).unwrap();
    let blocks = vec![
        paragraph("From Word"),
        Block::Image(Image {
            hash: photo.hash.clone(),
            alt: "A photo".into(),
            width: None,
        }),
    ];
    let mut pictures = std::collections::HashMap::new();
    pictures.insert(photo.hash.clone(), photo.clone());
    let word =
        accordion_docs::docx::write(&blocks, &Default::default(), None, "Report", &pictures, &[]);
    let file = alex
        .create_file(my_drive, "Report.docx", word.as_slice())
        .await
        .unwrap();

    // Nobody else can open it with Docs.
    let request = |item: &ItemId, confirmed: bool| json!({ "item": item, "confirmed": confirmed });
    let (status, _) = server
        .api(
            &server.sam,
            "POST",
            "/imports",
            Some(request(&file.id, false)),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Nothing is lost, so the document is made at once, beside the file.
    let (status, made) = server
        .api(
            &server.alex,
            "POST",
            "/imports",
            Some(request(&file.id, false)),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let document = &made["document"];
    assert_eq!(document["name"], "Report.html");
    assert_eq!(document["folder"], json!(my_drive));
    assert_eq!(made["report"]["lost"], json!([]));
    let id = document["id"].as_str().unwrap();
    let text = server.file_text(id).await;
    assert!(text.starts_with(MARKER), "{text}");
    let read = html::read_document(text.as_bytes());
    assert_eq!(read.blocks[0], paragraph("From Word"));
    // The photo was shrunk, as one added in the editor is.
    let Block::Image(image) = &read.blocks[1] else {
        panic!("{:?}", read.blocks);
    };
    assert_eq!(image.alt, "A photo");
    let kept = &read.pictures[&image.hash];
    assert_eq!((kept.width, kept.height), (2560, 1707));
    // Opening it again makes another, numbered.
    let (_, again) = server
        .api(
            &server.alex,
            "POST",
            "/imports",
            Some(request(&file.id, false)),
        )
        .await;
    assert_eq!(again["document"]["name"], "Report (2).html");

    // A page with a picture from elsewhere on the web loses it, so Docs
    // asks first.
    let page = alex
        .create_file(
            my_drive,
            "Saved page.htm",
            b"<h1>Saved</h1><p>Text <img src=\"https://example.com/a.png\"></p>".as_slice(),
        )
        .await
        .unwrap();
    let (status, asked) = server
        .api(
            &server.alex,
            "POST",
            "/imports",
            Some(request(&page.id, false)),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{asked}");
    assert_eq!(asked["document"], Value::Null);
    let (status, made) = server
        .api(
            &server.alex,
            "POST",
            "/imports",
            Some(request(&page.id, true)),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    assert_eq!(made["document"]["name"], "Saved page.html");

    // A viewer's document goes in their own Docs, since they can't add
    // beside the file.
    let notes = alex
        .create_file(
            my_drive,
            "Notes.md",
            b"# Notes\n\n- one\n- two\n".as_slice(),
        )
        .await
        .unwrap();
    server
        .as_person(&server.alex)
        .share(notes.id, server.sam.id, ShareLevel::Viewer, server.alex.id)
        .await
        .unwrap();
    let (status, mine) = server
        .api(
            &server.sam,
            "POST",
            "/imports",
            Some(request(&notes.id, false)),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{mine}");
    assert_eq!(mine["document"]["access"], "owner");
    assert_eq!(server.names(&server.sam).await, ["Notes.html"]);

    // Files Docs doesn't open, and broken ones, say so.
    let pdf = alex
        .create_file(my_drive, "Scan.pdf", b"%PDF-1.4".as_slice())
        .await
        .unwrap();
    let (status, _) = server
        .api(
            &server.alex,
            "POST",
            "/imports",
            Some(request(&pdf.id, false)),
        )
        .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let broken = alex
        .create_file(my_drive, "Broken.docx", b"not a zip".as_slice())
        .await
        .unwrap();
    let (status, problem) = server
        .api(
            &server.alex,
            "POST",
            "/imports",
            Some(request(&broken.id, false)),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
}

#[tokio::test]
async fn opens_opendocument_files_and_rich_text_with_docs() {
    let server = start().await;
    let my_drive = server.storage.top_folder(server.alex.id).await.unwrap();
    let alex = server.as_person(&server.alex);
    let request = |item: &ItemId| json!({ "item": item, "confirmed": false });

    // An OpenDocument file, as Docs' own download writes one.
    let blocks = vec![
        Block::Heading(
            1,
            Para::default(),
            vec![Inline::Text("Plan".into(), Marks::default())],
        ),
        paragraph("From LibreOffice"),
    ];
    let odt = accordion_docs::odt::write(
        &blocks,
        &Default::default(),
        None,
        "Plan",
        &Default::default(),
        &[],
    );
    let file = alex
        .create_file(my_drive, "Plan.odt", odt.as_slice())
        .await
        .unwrap();
    let (status, made) = server
        .api(&server.alex, "POST", "/imports", Some(request(&file.id)))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    assert_eq!(made["document"]["name"], "Plan.html");
    let read = html::read_document(
        server
            .file_text(made["document"]["id"].as_str().unwrap())
            .await
            .as_bytes(),
    );
    assert_eq!(read.blocks, blocks);

    // Rich Text goes through LibreOffice in the sandbox, then Docs reads
    // the Word file it makes.
    let rtf = alex
        .create_file(
            my_drive,
            "Letter.rtf",
            b"{\\rtf1\\ansi{\\fonttbl{\\f0 Arial;}}\\f0\\fs22 Dear Sam, {\\b thanks}.\\par}"
                .as_slice(),
        )
        .await
        .unwrap();
    let (status, made) = server
        .api(&server.alex, "POST", "/imports", Some(request(&rtf.id)))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    assert_eq!(made["document"]["name"], "Letter.html");
    let read = html::read_document(
        server
            .file_text(made["document"]["id"].as_str().unwrap())
            .await
            .as_bytes(),
    );
    let Block::Paragraph(_, inlines) = &read.blocks[0] else {
        panic!("{:?}", read.blocks);
    };
    assert!(inlines.iter().any(
        |inline| matches!(inline, Inline::Text(text, marks) if text == "thanks" && marks.bold)
    ));

    // A file named as an older Word file that isn't one isn't read as one.
    let fake = alex
        .create_file(my_drive, "Fake.doc", b"{\\rtf1 not Word}".as_slice())
        .await
        .unwrap();
    let (status, problem) = server
        .api(&server.alex, "POST", "/imports", Some(request(&fake.id)))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
}
