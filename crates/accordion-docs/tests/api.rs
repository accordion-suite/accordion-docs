//! Docs' API, mounted the way the server mounts it, with two people.

use std::time::Duration;

use accordion_core::coedit::{CoEditing, Timing};
use accordion_core::db::Database;
use accordion_core::events::{Actor, Events, Via};
use accordion_core::identity::{AuthMethod, Client, Identity, NewUser};
use accordion_core::live::Live;
use accordion_core::storage::{ShareLevel, Storage};
use accordion_core::texts::{self, Texts};
use accordion_core::thumbnails::{Thumbnails, Tools};
use accordion_core::transcripts::Transcripts;
use accordion_docs::Docs;
use accordion_docs::html::{self, MARKER};
use accordion_docs::model::{Block, Image, Inline, Marks};
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
    let services = Services {
        db: db.clone(),
        coediting: CoEditing::open(db.clone(), storage.clone(), identity.clone(), QUICK)
            .await
            .unwrap(),
        identity: identity.clone(),
        storage: storage.clone(),
        events: Events::open(db.clone()).await.unwrap(),
        thumbnails: thumbnails.clone(),
        texts,
        transcripts: Transcripts::canned(db.clone(), storage.clone(), dir.path(), |_| None)
            .await
            .unwrap(),
        live: Live::new(db, &identity),
        link_origin: watch::channel(None).1,
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
    Block::Paragraph(vec![Inline::Text(text.into(), Marks::default())])
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
            html::page(&[paragraph("Hi")], None, None, &Default::default()).as_bytes(),
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
async fn prints_a_document_on_its_paper() {
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
    // co-editing state in it. The print page has neither.
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

    let response = server
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/docs/documents/{id}/print"))
                .header("authorization", &server.alex.bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let policy = response.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(policy.starts_with("default-src 'none'"), "{policy}");
    assert!(policy.contains("frame-ancestors 'self'"), "{policy}");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let page = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        page.contains("@page{size:210mm 297mm;margin:72pt 72pt 72pt 72pt}"),
        "{page}"
    );
    assert!(page.contains("<p>Hello</p>"), "{page}");
    assert!(!page.contains("steal"), "{page}");
    assert!(!page.contains("application/x-accordion-docs"), "{page}");
    assert!(page.contains("<title>Untitled document</title>"), "{page}");

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

    // Only people who can open the document can print it.
    let (status, _) = server
        .api(&server.sam, "GET", &format!("/documents/{id}/print"), None)
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
        assert_eq!(alex.blocks(), [Block::Paragraph(Vec::new())]);
        let mut sam = Browser::open(address, &server.sam, &id).await;
        alex.add_paragraph("From Alex").await;
        sam.add_paragraph("From Sam").await;
        let both = [
            Block::Paragraph(Vec::new()),
            paragraph("From Alex"),
            paragraph("From Sam"),
        ];
        let other_order = [
            Block::Paragraph(Vec::new()),
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
            Block::Paragraph(Vec::new()),
            paragraph("Typed"),
            Block::Heading(
                2,
                vec![Inline::Text("Added elsewhere".into(), Marks::default())],
            ),
        ])
        .await;
        assert_eq!(file_blocks(&server, &id).await, alex.blocks());
    }

    #[tokio::test]
    async fn viewers_watch_but_their_edits_dont_land() {
        let server = start().await;
        let address = serve(&server).await;
        let id = server.new_document(&server.alex).await;
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
        let mut sam = Browser::open(address, &server.sam, &id).await;
        sam.add_paragraph("Not allowed").await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(
            file_blocks(&server, &id).await,
            [Block::Paragraph(Vec::new())]
        );
    }
}
