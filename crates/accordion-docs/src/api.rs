//! Docs' JSON API, at `/api/docs`, for the web app. It lists the signed-in
//! person's documents and the ones shared with them, makes new ones, and
//! opens a document for editing together over `/documents/{id}/live`.
//!
//! Renaming, moving, sharing, and the trash go through Drive's API, since a
//! document is a file like any other.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use accordion_core::identity::SignedIn;
use accordion_core::storage::{Access, Error, Item, ItemKind, Storage};
use accordion_module::{ApiError, Authenticated, coedit};
use accordion_types::{ItemId, UserId};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use crate::format::{DocFormat, new_document};
use crate::{DocsState, PLACE, html};

pub(crate) fn routes(state: DocsState) -> Router {
    Router::new()
        .route("/documents", get(list).post(create))
        .route("/documents/{id}", get(open))
        .route("/documents/{id}/live", get(live))
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
    /// "owner", "editor", or "viewer".
    access: &'static str,
    /// Who owns it, for a document someone shared.
    owner: Option<String>,
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
        .map(|item| DocumentView {
            id: item.id,
            name: item.name,
            folder: item.parent,
            modified_at: item.modified_at,
            access: "owner",
            owner: None,
        })
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
            documents.push(DocumentView {
                id: item.id,
                name: item.name,
                folder: item.parent,
                modified_at: item.modified_at,
                access,
                owner,
            });
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
    let contents = new_document();
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
        Json(DocumentView {
            id: item.id,
            name: item.name,
            folder: item.parent,
            modified_at: item.modified_at,
            access: "owner",
            owner: None,
        }),
    ))
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
    Ok(Json(DocumentView {
        id: item.id,
        name: item.name,
        folder: item.parent,
        modified_at: item.modified_at,
        access: access_name(access),
        owner,
    }))
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
    reach(&state, &me, id).await?;
    let connection = state
        .coediting
        .join(id, me.user.id, Some(me.session.id), Arc::new(DocFormat))
        .await?;
    Ok(coedit::upgrade(socket, connection))
}
