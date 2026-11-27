//! Docs' JSON API, at `/api/docs`, for the web app. It lists the signed-in
//! person's documents and the ones shared with them, makes new ones, and
//! opens a document for editing together over `/documents/{id}/live`.
//!
//! Renaming, moving, sharing, and the trash go through Drive's API, since a
//! document is a file like any other.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use accordion_core::identity::SignedIn;
use accordion_core::storage::ContentHash;
use accordion_core::storage::{Access, Error, Item, ItemKind, Storage};
use accordion_core::thumbnails::{ConvertError, PictureError, ThumbnailKind};
use accordion_module::{ApiError, Authenticated, coedit};
use accordion_types::{ItemId, UserId};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State, WebSocketUpgrade};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use crate::format::{self, DocFormat, new_document};
use crate::import::{self, Lost, Report};
use crate::model;
use crate::model::{PageSetup, PageSize};
use crate::picture::{self, Picture, Pictures};
use crate::{DocsState, PLACE, docx, html, markdown, odt};

mod comments;
mod versions;

pub(crate) fn routes(state: DocsState) -> Router {
    Router::new()
        .route("/documents", get(list).post(create))
        .route("/documents/{id}", get(open))
        .route("/documents/{id}/copy", post(copy))
        .route("/documents/{id}/live", get(live))
        .route("/documents/{id}/download", get(download))
        .route(
            "/documents/{id}/images",
            post(add_image).layer(DefaultBodyLimit::max(MAX_UPLOAD)),
        )
        .route("/documents/{id}/images/{hash}", get(image))
        .route(
            "/documents/{id}/comments",
            get(comments::list).post(comments::start),
        )
        .route("/documents/{id}/people", get(comments::people))
        .route(
            "/documents/{id}/comments/{thread}",
            post(comments::reply)
                .patch(comments::resolve)
                .delete(comments::remove_thread),
        )
        .route(
            "/documents/{id}/comments/{thread}/{comment}",
            patch(comments::edit).delete(comments::remove),
        )
        .route("/documents/{id}/versions", get(versions::list))
        .route("/documents/{id}/versions/{version}", get(versions::show))
        .route(
            "/documents/{id}/versions/{version}/name",
            put(versions::name).delete(versions::unname),
        )
        .route(
            "/documents/{id}/versions/{version}/restore",
            post(versions::restore),
        )
        .route(
            "/documents/{id}/versions/{version}/copy",
            post(versions::copy),
        )
        .route("/imports", post(import))
        .with_state(state)
}

/// The name a new document gets, numbered if it's taken.
const NEW_NAME: &str = "Untitled document";

/// A document, as the web app lists it.
#[derive(Serialize)]
struct DocumentView {
    id: ItemId,
    /// The file's name, with `.html`.
    name: String,
    /// The folder it's in.
    folder: Option<ItemId>,
    modified_at: String,
    /// "owner", "editor", "commenter", or "viewer".
    access: &'static str,
    /// Who owns it, for a document someone shared.
    owner: Option<String>,
    /// The contents' SHA-256, which the thumbnail's address carries, so
    /// browsers fetch a new one for each version.
    sha256: Option<String>,
    /// "page" once core has made the document's thumbnail, its first lines
    /// with their headings and styles, which Drive's
    /// `/items/{id}/thumbnail` sends.
    thumbnail: Option<&'static str>,
    /// Changes each time core makes the thumbnail again.
    thumbnail_version: Option<String>,
}

impl DocumentView {
    fn new(state: &DocsState, item: Item, access: &'static str, owner: Option<String>) -> Self {
        let hash = match &item.kind {
            ItemKind::File { blob, .. } => Some(blob.hash),
            ItemKind::Folder => None,
        };
        let found = hash
            .and_then(|hash| state.thumbnails.find(hash))
            .filter(|found| found.kind == ThumbnailKind::Page);
        Self {
            id: item.id,
            name: item.name,
            folder: item.parent,
            modified_at: item.modified_at,
            access,
            owner,
            sha256: hash.map(|hash| hash.to_hex()),
            thumbnail: found.is_some().then_some("page"),
            thumbnail_version: found.map(|found| found.version()),
        }
    }
}

#[derive(Serialize)]
struct ListView {
    /// The top of the person's Docs place, where new documents go.
    top: ItemId,
    /// Most recently changed first.
    documents: Vec<DocumentView>,
}

fn access_name(access: Access) -> &'static str {
    match access {
        Access::Owner => "owner",
        Access::Editor => "editor",
        Access::Commenter => "commenter",
        Access::Viewer => "viewer",
    }
}

/// Whether `item` is a document: an HTML file that starts with Docs'
/// marker. Only the start of the file is read.
async fn is_document(storage: &Storage, item: &Item) -> Result<bool, Error> {
    let ItemKind::File { blob, .. } = &item.kind else {
        return Ok(false);
    };
    if !item.name.to_ascii_lowercase().ends_with(".html") {
        return Ok(false);
    }
    let file = storage.open_contents(blob.hash).await?;
    let mut start = Vec::with_capacity(html::MARKER.len());
    file.take(html::MARKER.len() as u64)
        .read_to_end(&mut start)
        .await
        .map_err(Error::Io)?;
    Ok(html::is_document(&start))
}

/// Every document under `folder`, depth first.
async fn documents_in(storage: &Storage, folder: ItemId) -> Result<Vec<Item>, Error> {
    let mut found = Vec::new();
    let mut waiting = vec![folder];
    while let Some(folder) = waiting.pop() {
        for child in storage.children(folder).await? {
            match child.kind {
                ItemKind::Folder => waiting.push(child.id),
                ItemKind::File { .. } => {
                    if is_document(storage, &child).await? {
                        found.push(child);
                    }
                }
            }
        }
    }
    Ok(found)
}

/// `GET /documents`: the person's documents in their Docs place, and the
/// documents others shared with them, wherever those are.
async fn list(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
) -> Result<Json<ListView>, ApiError> {
    let user = me.user.id;
    let top = state.storage.place_folder(user, PLACE.id).await?;
    let mut documents: Vec<DocumentView> = documents_in(&state.storage, top)
        .await?
        .into_iter()
        .map(|item| DocumentView::new(&state, item, "owner", None))
        .collect();

    let mut seen: HashSet<ItemId> = documents.iter().map(|document| document.id).collect();
    let mut names: HashMap<UserId, Option<String>> = HashMap::new();
    for shared in state.storage.shared_with(user).await? {
        let access = access_name(Access::from(shared.share.level));
        let items = match shared.item.kind {
            ItemKind::Folder => documents_in(&state.storage, shared.item.id).await?,
            ItemKind::File { .. } => {
                if is_document(&state.storage, &shared.item).await? {
                    vec![shared.item]
                } else {
                    Vec::new()
                }
            }
        };
        for item in items {
            if !seen.insert(item.id) {
                continue;
            }
            let owner = match names.get(&item.owner) {
                Some(name) => name.clone(),
                None => {
                    let name = state
                        .identity
                        .user(item.owner)
                        .await
                        .ok()
                        .flatten()
                        .map(|user| user.display_name);
                    names.insert(item.owner, name.clone());
                    name
                }
            };
            documents.push(DocumentView::new(&state, item, access, owner));
        }
    }
    documents.sort_by(|a, b| b.modified_at.cmp(&a.modified_at).then(a.name.cmp(&b.name)));
    Ok(Json(ListView { top, documents }))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct NewDocument {
    /// A folder in the person's Docs place. The top of the place if not
    /// given.
    folder: Option<ItemId>,
    /// The paper size, which the web app picks by the person's region.
    /// Letter if not given.
    page: Option<PageSize>,
    /// The co-editing state of a document someone started in the browser,
    /// in base64: the web app makes a document's file only once something
    /// in it changes, so a document opened and left blank leaves nothing
    /// behind.
    state: Option<String>,
}

/// `POST /documents`: a new, empty document, named "Untitled document",
/// numbered if that's taken.
async fn create(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    request: Option<Json<NewDocument>>,
) -> Result<(StatusCode, Json<DocumentView>), ApiError> {
    let request = request.map(|Json(request)| request).unwrap_or_default();
    let user = me.user.id;
    let top = state.storage.place_folder(user, PLACE.id).await?;
    let folder = match request.folder {
        Some(folder) if folder != top => {
            let inside = state.storage.path_within(top, folder).await?.is_some();
            let is_folder = matches!(
                state.storage.item(folder).await?,
                Some(Item {
                    kind: ItemKind::Folder,
                    ..
                })
            );
            if !inside || !is_folder {
                return Err(ApiError::not_found());
            }
            folder
        }
        _ => top,
    };
    let storage = state.storage.acting_as(accordion_module::auth::actor(&me));
    let setup = PageSetup::new(request.page.unwrap_or_default());
    let contents = match request.state {
        None => new_document(&setup),
        Some(state_text) => {
            let update = base64::engine::general_purpose::STANDARD
                .decode(state_text)
                .map_err(|_| unreadable_draft())?;
            let format = DocFormat {
                images: state.images.clone(),
                comments: None,
            };
            // Writing reads the pictures it shows from the database.
            tokio::task::spawn_blocking(move || format.file_from_draft(&update, &setup))
                .await
                .map_err(|err| {
                    tracing::error!("couldn't write a new document: {err}");
                    ApiError::server_problem()
                })?
                .ok_or_else(unreadable_draft)?
        }
    };
    let mut created = None;
    for number in 1..=100 {
        let name = match number {
            1 => format!("{NEW_NAME}.html"),
            n => format!("{NEW_NAME} ({n}).html"),
        };
        match storage
            .create_file(folder, &name, contents.as_slice())
            .await
        {
            Err(Error::NameTaken { .. }) => continue,
            result => {
                created = Some(result?);
                break;
            }
        }
    }
    let item = created.ok_or_else(|| {
        ApiError::new(
            StatusCode::CONFLICT,
            "This folder has too many untitled documents. Rename some, then try again.",
        )
    })?;
    Ok((
        StatusCode::CREATED,
        Json(DocumentView::new(&state, item, "owner", None)),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CopyRequest {
    /// The copy's name, such as "Copy of Plans.html". It gets `.html` if it
    /// doesn't end with it, and a number if it's taken.
    name: String,
    /// Whether the copy keeps the comments. Google Docs leaves them out
    /// unless asked.
    #[serde(default)]
    comments: bool,
}

/// `POST /documents/{id}/copy`: a copy of a document with everyone's latest
/// edits, as Google Docs' **Make a copy**. It goes beside the original if
/// the person can add to that folder, and to the top of their Docs place
/// if not, so a viewer keeps a copy of their own.
async fn copy(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path(id): Path<ItemId>,
    Json(request): Json<CopyRequest>,
) -> Result<(StatusCode, Json<DocumentView>), ApiError> {
    let (item, _) = reach(&state, &me, id).await?;
    let stem = copy_stem(&request.name)?;
    // The copy has what's been typed up to now.
    let (_, contents) = current_contents(&state, id, item.clone()).await?;
    // Left out, the comments go from the copy's file too.
    let without_comments = if request.comments || html::comments_in(&contents).is_empty() {
        None
    } else {
        html::with_comments(&contents, &[])
    };
    make_copy(&state, &me, &item, stem, without_comments).await
}

/// A copy's name without `.html`, which it gets back when it's made.
fn copy_stem(name: &str) -> Result<&str, ApiError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "Give the copy a name, then try again.",
        ));
    }
    Ok(match name.len().checked_sub(".html".len()) {
        Some(end) if name.is_char_boundary(end) && name[end..].eq_ignore_ascii_case(".html") => {
            &name[..end]
        }
        _ => name,
    })
}

/// Makes a copy of `item` named `stem`, numbered if that's taken, with
/// `contents` as its file, or the item's own file if `None`. It goes beside
/// the original if the person can add to that folder, and to the top of
/// their Docs place if not.
async fn make_copy(
    state: &DocsState,
    me: &SignedIn,
    item: &Item,
    stem: &str,
    without_comments: Option<Vec<u8>>,
) -> Result<(StatusCode, Json<DocumentView>), ApiError> {
    let id = item.id;
    let here = match item.parent {
        Some(parent) => state.storage.access(me.user.id, parent).await?,
        None => None,
    };
    let folder = match (item.parent, here) {
        (Some(parent), Some(access)) if access >= Access::Editor => parent,
        _ => state.storage.place_folder(me.user.id, PLACE.id).await?,
    };
    let storage = state.storage.acting_as(accordion_module::auth::actor(me));
    let mut copied = None;
    for number in 1..=100 {
        let name = match number {
            1 => format!("{stem}.html"),
            n => format!("{stem} ({n}).html"),
        };
        let made = match &without_comments {
            Some(contents) => {
                storage
                    .create_file(folder, &name, contents.as_slice())
                    .await
            }
            None => storage.copy_file(id, folder, &name).await,
        };
        match made {
            Err(Error::NameTaken { .. }) => continue,
            result => {
                copied = Some(result?);
                break;
            }
        }
    }
    let copy = copied.ok_or_else(|| {
        ApiError::new(
            StatusCode::CONFLICT,
            "This folder already has too many documents with that name. Choose another name, then try again.",
        )
    })?;
    let access = state
        .storage
        .access(me.user.id, copy.id)
        .await?
        .unwrap_or(Access::Owner);
    let owner = if copy.owner == me.user.id {
        None
    } else {
        state
            .identity
            .user(copy.owner)
            .await
            .ok()
            .flatten()
            .map(|user| user.display_name)
    };
    Ok((
        StatusCode::CREATED,
        Json(DocumentView::new(state, copy, access_name(access), owner)),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportRequest {
    /// The file to open with Docs.
    item: ItemId,
    /// Whether the person has seen what won't come across and still wants
    /// the document. Without it, a file that would lose something isn't
    /// converted yet: the report comes back instead.
    #[serde(default)]
    confirmed: bool,
}

#[derive(Serialize)]
struct ImportView {
    /// The new document, once there is one.
    document: Option<DocumentView>,
    report: Report,
}

/// `POST /imports`: a new document from a Word or OpenDocument file, a web
/// page, Markdown, or plain text, as Google Docs' **Open with**. Older Word
/// files and Rich Text become Word files through LibreOffice first, where
/// it's installed. The file stays as it is;
/// the document goes beside it if the person can add to that folder, and
/// to the top of their Docs place if not. The file is read in the sandbox
/// (decision 1). When something wouldn't come across, such as text boxes,
/// the first request only reports it, and the web app asks before sending
/// `confirmed`.
async fn import(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Json(request): Json<ImportRequest>,
) -> Result<(StatusCode, Json<ImportView>), ApiError> {
    let id = request.item;
    state
        .storage
        .access(me.user.id, id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let item = state
        .storage
        .item(id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    if !state.storage.outside_trash(id).await? {
        return Err(ApiError::not_found());
    }
    let ItemKind::File { blob, .. } = &item.kind else {
        return Err(ApiError::not_found());
    };
    let extension = item
        .name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    let through_office = import::THROUGH_LIBREOFFICE.contains(&extension.as_str());
    if !import::EXTENSIONS.contains(&extension.as_str()) && !through_office {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Docs opens Word and OpenDocument files, Rich Text, web pages, Markdown, and text files. Choose one of those.",
        ));
    }
    if blob.size > import::MAX_FILE {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "This file is too large for Docs to open. Docs opens files up to 100 MB.",
        ));
    }
    let mut contents = read_contents(&state, blob.hash).await?;
    let mut extension = extension;
    if through_office {
        contents = match state.thumbnails.office_to_word(&contents, &extension).await {
            Ok(word) => word,
            Err(ConvertError::NotInstalled) => {
                return Err(ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    "Docs needs LibreOffice on the server to open this kind of file, and it isn't installed. Ask your admin to install it, or save the file as a .docx file in Word and open that.",
                ));
            }
            Err(ConvertError::Failed(why)) => {
                tracing::warn!(%id, %why, "LibreOffice couldn't convert a file to open with Docs");
                return Err(unreadable_file());
            }
        };
        extension = "docx".to_owned();
    }
    let output = state
        .thumbnails
        .run_command(&[import::COMMAND, &extension], &contents)
        .await
        .map_err(|why| {
            tracing::warn!(%id, %why, "couldn't open a file with Docs");
            unreadable_file()
        })?;
    let (report, page) = match output.iter().position(|byte| *byte == b'\n') {
        Some(end) => (
            serde_json::from_slice::<Report>(&output[..end]).map_err(|_| unreadable_file())?,
            &output[end + 1..],
        ),
        None => return Err(unreadable_file()),
    };
    if report.warns() && !request.confirmed {
        return Ok((
            StatusCode::OK,
            Json(ImportView {
                document: None,
                report,
            }),
        ));
    }
    let mut report = report;
    // The page goes through the schema again, so nothing the sandbox wrote
    // reaches the document unchecked.
    let html::Document {
        mut blocks,
        setup,
        pictures,
        mut parts,
    } = html::read_document(page);
    let threads = html::file_comments_in(page);
    // Pictures are checked and shrunk as pictures people add are.
    let mut kept = Pictures::new();
    let mut renamed: HashMap<String, Option<String>> = HashMap::new();
    for (hash, picture) in pictures {
        let shrunk = match state
            .thumbnails
            .shrink_picture(&picture.contents, PICTURE_SIZE)
            .await
        {
            Ok(shrunk) => Picture::read(shrunk.contents),
            Err(err) => {
                tracing::warn!(%id, %err, "couldn't shrink a picture in a file opened with Docs");
                None
            }
        };
        match shrunk {
            Some(picture) => {
                renamed.insert(hash, Some(picture.hash.clone()));
                kept.insert(picture.hash.clone(), picture);
            }
            None => {
                renamed.insert(hash, None);
                report.lost.insert(Lost::Pictures);
            }
        }
    }
    model::rename_images(&mut blocks, &renamed);
    for part in [
        &mut parts.header,
        &mut parts.footer,
        &mut parts.first_header,
        &mut parts.first_footer,
    ] {
        model::rename_images(part, &renamed);
    }
    let mut blocks = model::normalize(blocks);
    for part in [
        &mut parts.header,
        &mut parts.footer,
        &mut parts.first_header,
        &mut parts.first_footer,
    ] {
        model::without_comment_marks(part);
    }
    let parts = parts.normalized();
    // Comments need positions in the co-editing state, so a document with
    // them gets its state now. One without them gets its state when it's
    // first opened, as a new document does.
    let file = if threads.is_empty() {
        model::without_comment_marks(&mut blocks);
        let blocks = model::normalize(blocks);
        html::page(&blocks, &parts, setup.as_ref(), None, &kept).into_bytes()
    } else {
        format::file_with_comments(&blocks, &parts, setup.as_ref(), &kept, threads)
    };
    if file.len() as u64 > MAX_FILE {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "This file's pictures are too large for one document. A document can be at most 50 MB.",
        ));
    }

    // Beside the file, as Google Docs puts it, or in the person's own Docs.
    let here = match item.parent {
        Some(parent) => state.storage.access(me.user.id, parent).await?,
        None => None,
    };
    let folder = match (item.parent, here) {
        (Some(parent), Some(access)) if access >= Access::Editor => parent,
        _ => state.storage.place_folder(me.user.id, PLACE.id).await?,
    };
    let stem = stem(&item.name).to_owned();
    let stem = if stem.is_empty() {
        NEW_NAME.to_owned()
    } else {
        stem
    };
    let storage = state.storage.acting_as(accordion_module::auth::actor(&me));
    let mut created = None;
    for number in 1..=100 {
        let name = match number {
            1 => format!("{stem}.html"),
            n => format!("{stem} ({n}).html"),
        };
        match storage.create_file(folder, &name, file.as_slice()).await {
            Err(Error::NameTaken { .. }) => continue,
            result => {
                created = Some(result?);
                break;
            }
        }
    }
    let document = created.ok_or_else(|| {
        ApiError::new(
            StatusCode::CONFLICT,
            "This folder already has too many documents with this file's name. Rename the file, then try again.",
        )
    })?;
    let access = state
        .storage
        .access(me.user.id, document.id)
        .await?
        .unwrap_or(Access::Owner);
    let owner = if document.owner == me.user.id {
        None
    } else {
        state
            .identity
            .user(document.owner)
            .await
            .ok()
            .flatten()
            .map(|user| user.display_name)
    };
    Ok((
        StatusCode::CREATED,
        Json(ImportView {
            document: Some(DocumentView::new(
                &state,
                document,
                access_name(access),
                owner,
            )),
            report,
        }),
    ))
}

fn unreadable_file() -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "Docs couldn't read this file. It may be damaged, or not the kind of file its name says.",
    )
}

fn unreadable_draft() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "Docs couldn't read this document's changes. Reload the page, then try again.",
    )
}

/// A document `me` can open, and how far they can go, or "not found", so
/// nobody learns what exists.
async fn reach(state: &DocsState, me: &SignedIn, id: ItemId) -> Result<(Item, Access), ApiError> {
    let access = state
        .storage
        .access(me.user.id, id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let item = state
        .storage
        .item(id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    if !state.storage.outside_trash(id).await? || !is_document(&state.storage, &item).await? {
        return Err(ApiError::not_found());
    }
    Ok((item, access))
}

/// `GET /documents/{id}`: a document's name and the person's access, for
/// the editor's title bar, and for Drive to know a file opens in Docs.
async fn open(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path(id): Path<ItemId>,
) -> Result<Json<DocumentView>, ApiError> {
    let (item, access) = reach(&state, &me, id).await?;
    let owner = if item.owner == me.user.id {
        None
    } else {
        state
            .identity
            .user(item.owner)
            .await
            .ok()
            .flatten()
            .map(|user| user.display_name)
    };
    Ok(Json(DocumentView::new(
        &state,
        item,
        access_name(access),
        owner,
    )))
}

/// `GET /documents/{id}/live`: a WebSocket for editing a document together
/// with everyone else who has it open (decision 30). Viewers see the edits
/// but can't make them.
async fn live(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path(id): Path<ItemId>,
    socket: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let (item, _) = reach(&state, &me, id).await?;
    // Saving needs every picture in the file, which core's own copy of the
    // document refers to only by hash.
    if let ItemKind::File { blob, .. } = &item.kind {
        let file = blob.hash.to_string();
        if !state.images.knows_file(&file) {
            let contents = read_contents(&state, blob.hash).await?;
            state
                .images
                .take_in_file(&file, &contents)
                .await
                .map_err(pictures_problem)?;
        }
    }
    // Saving writes the comments into the file, so a file's own are taken
    // in before anyone can change it.
    comments::threads_of(&state, &item).await?;
    let format = DocFormat {
        images: state.images.clone(),
        comments: Some((state.comments.clone(), id)),
    };
    let connection = state
        .coediting
        .join(id, me.user.id, Some(me.session.id), Arc::new(format))
        .await?;
    Ok(coedit::upgrade(socket, connection))
}

/// A document's file with everyone's latest edits in it: an open
/// document is saved first.
async fn current_contents(
    state: &DocsState,
    id: ItemId,
    item: Item,
) -> Result<(Item, Vec<u8>), ApiError> {
    state.coediting.save_now(id).await;
    // Saving may have made a new version, so the item is read again.
    let item = state.storage.item(id).await?.unwrap_or(item);
    let ItemKind::File { blob, .. } = &item.kind else {
        return Err(ApiError::not_found());
    };
    let contents = read_contents(state, blob.hash).await?;
    Ok((item, contents))
}

async fn read_contents(state: &DocsState, hash: ContentHash) -> Result<Vec<u8>, ApiError> {
    let mut contents = Vec::new();
    state
        .storage
        .open_contents(hash)
        .await?
        .read_to_end(&mut contents)
        .await
        .map_err(Error::Io)?;
    Ok(contents)
}

fn pictures_problem(err: accordion_core::db::Error) -> ApiError {
    tracing::error!("couldn't read or keep documents' pictures: {err}");
    ApiError::server_problem()
}

/// The most a picture someone adds may be, before it's shrunk: enough for
/// a photo from any phone.
const MAX_UPLOAD: usize = 40 * 1024 * 1024;

/// The longer side of a picture in a document, in pixels.
const PICTURE_SIZE: u32 = 2560;

/// The largest a document's file may grow to with its pictures, as core's
/// co-editing limit.
const MAX_FILE: u64 = 50 * 1024 * 1024;

#[derive(Serialize)]
struct AddedImage {
    /// What the editor's picture keeps as its `src`.
    hash: String,
    width: u32,
    height: u32,
}

/// `POST /documents/{id}/images`: a picture to add to a document, as its
/// bytes. The sandboxed worker checks it and shrinks it to 2,560 pixels on
/// its longer side (decision 30). It's kept until the document is saved
/// with it, and the editor then adds it by its hash.
async fn add_image(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path(id): Path<ItemId>,
    body: Bytes,
) -> Result<(StatusCode, Json<AddedImage>), ApiError> {
    let (item, access) = reach(&state, &me, id).await?;
    if access < Access::Editor {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "You can view this document but not change it. Ask its owner to make you an editor.",
        ));
    }
    if body.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "This picture is empty. Choose another one.",
        ));
    }
    let shrunk = state
        .thumbnails
        .shrink_picture(&body, PICTURE_SIZE)
        .await
        .map_err(|err| match err {
            PictureError::NotAPicture => ApiError::new(
                StatusCode::BAD_REQUEST,
                "Docs takes JPEG, PNG, GIF, and WebP pictures. Choose one of those.",
            ),
            PictureError::Failed(why) => {
                tracing::warn!(%id, %why, "couldn't add a picture to a document");
                ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Accordion couldn't read this picture. It may be damaged. Choose another one.",
                )
            }
        })?;
    let picture = Picture::read(shrunk.contents).ok_or_else(|| {
        tracing::error!(%id, "a shrunk picture isn't one Docs reads");
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Accordion couldn't read this picture. It may be damaged. Choose another one.",
        )
    })?;
    // Written into the file as text, a picture takes a third more room.
    let size = match &item.kind {
        ItemKind::File { blob, .. } => blob.size,
        ItemKind::Folder => 0,
    };
    if size + picture.contents.len() as u64 / 3 * 4 > MAX_FILE {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "This document has no room for another picture. A document can be at most 50 MB. \
             Make a new document for more pictures.",
        ));
    }
    state
        .images
        .add(picture.clone())
        .await
        .map_err(pictures_problem)?;
    Ok((
        StatusCode::CREATED,
        Json(AddedImage {
            hash: picture.hash,
            width: picture.width,
            height: picture.height,
        }),
    ))
}

/// `GET /documents/{id}/images/{hash}`: a picture in a document, for the
/// editor to show. Anyone who can open the document can see it. Its
/// address names its contents, so browsers keep it as long as they like.
async fn image(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path((id, hash)): Path<(ItemId, String)>,
) -> Result<Response, ApiError> {
    let (item, _) = reach(&state, &me, id).await?;
    if !picture::is_hash(&hash) {
        return Err(ApiError::not_found());
    }
    let mut found = state.images.get(&hash).await.map_err(pictures_problem)?;
    if found.is_none() {
        // Left out after months unused: the file still has it.
        if let ItemKind::File { blob, .. } = &item.kind {
            let contents = read_contents(&state, blob.hash).await?;
            state
                .images
                .take_in_file(&blob.hash.to_string(), &contents)
                .await
                .map_err(pictures_problem)?;
            found = state.images.get(&hash).await.map_err(pictures_problem)?;
        }
    }
    let picture = found.ok_or_else(ApiError::not_found)?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(picture.content_type),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=31536000, immutable"),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("sandbox"),
            ),
        ],
        Bytes::from_owner(picture.contents.clone()),
    )
        .into_response())
}

/// The name without its extension, such as "Trip plan" for
/// "Trip plan.html".
fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(stem, _)| stem).trim()
}

#[derive(Deserialize)]
struct DownloadQuery {
    /// The format, by its file name extension: "html" (the file itself),
    /// "md", "docx", or "odt".
    #[serde(rename = "as")]
    format: String,
    /// The page each heading in a table of contents is on, as the editor
    /// lays the pages out, such as "1.1.2", for Word's and OpenDocument's
    /// page numbers.
    pages: Option<String>,
}

/// The most page numbers a download takes.
const MAX_CONTENTS_PAGES: usize = 10_000;

/// Page numbers such as "1.1.2", or none if any isn't one.
fn contents_pages(text: Option<&str>) -> Vec<u32> {
    let Some(text) = text.filter(|text| !text.is_empty()) else {
        return Vec::new();
    };
    let pages: Option<Vec<u32>> = text
        .split('.')
        .take(MAX_CONTENTS_PAGES + 1)
        .map(|page| {
            page.parse()
                .ok()
                .filter(|page| (1..=100_000).contains(page))
        })
        .collect();
    pages
        .filter(|pages| pages.len() <= MAX_CONTENTS_PAGES)
        .unwrap_or_default()
}

/// `GET /documents/{id}/download?as=html`: the document to keep, with
/// everyone's latest edits in it, as the file itself or as Markdown, Word,
/// or OpenDocument (decision 30). PDF comes from printing.
async fn download(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path(id): Path<ItemId>,
    Query(query): Query<DownloadQuery>,
) -> Result<Response, ApiError> {
    let (item, _) = reach(&state, &me, id).await?;
    let kind = match query.format.as_str() {
        "html" => "text/html; charset=utf-8",
        "md" => "text/markdown; charset=utf-8",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "odt" => odt::MIME_TYPE,
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "Accordion can't download a document in that format.",
            ));
        }
    };
    let (item, contents) = current_contents(&state, id, item).await?;
    let threads = if matches!(query.format.as_str(), "docx" | "odt") {
        comments::threads_of(&state, &item).await?
    } else {
        Vec::new()
    };
    let title = stem(&item.name).to_owned();
    let name = format!("{title}.{}", query.format);
    let file = if query.format == "html" {
        contents
    } else {
        // Writing a big document takes a moment, so it's done off the
        // threads that answer requests.
        let format = query.format;
        let pages = contents_pages(query.pages.as_deref());
        tokio::task::spawn_blocking(move || {
            let html::Document {
                mut blocks,
                setup,
                pictures,
                parts,
            } = html::read_document(&contents);
            if !threads.is_empty()
                && let Some(marked) = format::blocks_with_comments(&contents, &blocks, &threads)
            {
                blocks = marked;
            }
            match format.as_str() {
                "md" => markdown::write(&blocks, &pictures).into_bytes(),
                "docx" => docx::write(
                    &blocks,
                    &parts,
                    setup.as_ref(),
                    &title,
                    &pictures,
                    &pages,
                    &threads,
                ),
                _ => odt::write(
                    &blocks,
                    &parts,
                    setup.as_ref(),
                    &title,
                    &pictures,
                    &pages,
                    &threads,
                ),
            }
        })
        .await
        .map_err(|err| {
            tracing::error!("couldn't write document {id} for download: {err}");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Accordion couldn't make the download. Try again in a moment.",
            )
        })?
    };
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(kind)),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("sandbox"),
            ),
            (
                header::CONTENT_DISPOSITION,
                disposition("attachment", &name),
            ),
        ],
        file,
    )
        .into_response())
}

/// A `Content-Disposition` header, "attachment" or "inline", that names the
/// file. `filename*` carries the exact name in any script (RFC 6266), and
/// `filename` is a plain ASCII stand-in for clients that don't read it.
/// The same as Drive's.
fn disposition(kind: &str, name: &str) -> HeaderValue {
    let plain: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    HeaderValue::try_from(format!(
        "{kind}; filename=\"{plain}\"; filename*=UTF-8''{encoded}"
    ))
    .expect("the name is ASCII once encoded")
}
