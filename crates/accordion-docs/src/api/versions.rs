//! Version history in `/api/docs` (decision 30): a document's versions with
//! who made each, one version to show, names, restoring, and copies.
//! Versions are Drive's: each editing session saves one, and core keeps
//! them. Only editors see them, as in Drive's **Versions** dialog.

use std::collections::HashMap;

use accordion_core::identity::SignedIn;
use accordion_core::storage::{Access, Error, Item, Version};
use accordion_module::{ApiError, Authenticated};
use accordion_types::{ItemId, UserId, VersionId};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use super::{DocumentView, copy_stem, make_copy, reach};
use crate::DocsState;
use crate::format;

#[derive(Serialize)]
pub(super) struct VersionsView {
    /// Newest first. The first is the current version.
    versions: Vec<VersionView>,
}

/// A version, as Drive's API shows it.
#[derive(Serialize)]
pub(super) struct VersionView {
    id: VersionId,
    number: u32,
    size: u64,
    created_at: String,
    /// The name someone gave it, which keeps it past the retention period.
    name: Option<String>,
    /// The people whose changes went into it, in the order they first did.
    people: Vec<VersionPerson>,
}

#[derive(Clone, Serialize)]
struct VersionPerson {
    user: UserId,
    display_name: String,
}

/// The versions as the web app shows them, with people's names looked up
/// once each. People whose accounts were deleted are left out.
async fn views(state: &DocsState, versions: Vec<Version>) -> Result<Vec<VersionView>, ApiError> {
    let mut names: HashMap<UserId, Option<VersionPerson>> = HashMap::new();
    let mut views = Vec::with_capacity(versions.len());
    for version in versions {
        let mut people = Vec::with_capacity(version.people.len());
        for user in version.people {
            let person = match names.get(&user) {
                Some(person) => person.clone(),
                None => {
                    let person = state.identity.user(user).await?.map(|found| VersionPerson {
                        user,
                        display_name: found.display_name,
                    });
                    names.insert(user, person.clone());
                    person
                }
            };
            people.extend(person);
        }
        views.push(VersionView {
            id: version.id,
            number: version.number,
            size: version.blob.size,
            created_at: version.created_at,
            name: version.name,
            people,
        });
    }
    Ok(views)
}

/// A document `me` can edit, since only editors see its versions.
async fn editing(state: &DocsState, me: &SignedIn, id: ItemId) -> Result<Item, ApiError> {
    let (item, access) = reach(state, me, id).await?;
    if access < Access::Editor {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "Only editors can see this document's versions. Ask its owner to make you an editor.",
        ));
    }
    Ok(item)
}

async fn newest(state: &DocsState, id: ItemId) -> Result<Version, ApiError> {
    state
        .storage
        .versions(id)
        .await?
        .into_iter()
        .next()
        .ok_or_else(ApiError::not_found)
}

async fn contents_of(
    state: &DocsState,
    id: ItemId,
    version: VersionId,
) -> Result<Vec<u8>, ApiError> {
    let mut contents = Vec::new();
    state
        .storage
        .open_version(id, version)
        .await?
        .read_to_end(&mut contents)
        .await
        .map_err(Error::Io)?;
    Ok(contents)
}

/// Runs `work` off the threads that answer requests, since reading a big
/// document takes a moment.
async fn off_thread<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(work).await.map_err(|err| {
        tracing::error!("couldn't read a version of a document: {err}");
        ApiError::server_problem()
    })
}

/// `GET /documents/{id}/versions`: every version, newest first, with who
/// made each. Typing is saved first, so the current version is what
/// everyone has on screen.
pub(super) async fn list(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path(id): Path<ItemId>,
) -> Result<Json<VersionsView>, ApiError> {
    editing(&state, &me, id).await?;
    state.coediting.save_now(id).await;
    let versions = state.storage.versions(id).await?;
    Ok(Json(VersionsView {
        versions: views(&state, versions).await?,
    }))
}

/// `GET /documents/{id}/versions/{version}`: a version to show, as a
/// co-editing state the editor reads (a Yjs update), made from the
/// version's file through the schema, so nothing in an old file reaches the
/// page unchecked. Its pictures are kept for the page to show.
pub(super) async fn show(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path((id, version)): Path<(ItemId, VersionId)>,
) -> Result<Response, ApiError> {
    editing(&state, &me, id).await?;
    let contents = contents_of(&state, id, version).await?;
    let (update, pictures) = off_thread(move || format::version_state(&contents)).await?;
    state.images.remember(pictures);
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        ],
        update,
    )
        .into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VersionName {
    /// Up to 100 characters. Empty removes the name.
    name: String,
}

/// `PUT /documents/{id}/versions/{version}/name`: names a version. As
/// `current`, typing is saved first and the newest version is named, so the
/// name covers what's on screen, as Google Docs' **Name current version**.
pub(super) async fn name(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path((id, version)): Path<(ItemId, String)>,
    Json(body): Json<VersionName>,
) -> Result<Json<VersionView>, ApiError> {
    named(&state, &me, id, &version, Some(&body.name)).await
}

/// `DELETE /documents/{id}/versions/{version}/name`: removes a version's
/// name, so it expires on the usual schedule.
pub(super) async fn unname(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path((id, version)): Path<(ItemId, String)>,
) -> Result<Json<VersionView>, ApiError> {
    named(&state, &me, id, &version, None).await
}

async fn named(
    state: &DocsState,
    me: &SignedIn,
    id: ItemId,
    version: &str,
    name: Option<&str>,
) -> Result<Json<VersionView>, ApiError> {
    editing(state, me, id).await?;
    let version = if version == "current" {
        state.coediting.save_now(id).await;
        newest(state, id).await?.id
    } else {
        version.parse().map_err(|_| ApiError::not_found())?
    };
    let named = state.storage.name_version(id, version, name).await?;
    let mut views = views(state, vec![named]).await?;
    Ok(Json(views.remove(0)))
}

#[derive(Serialize)]
pub(super) struct Restored {
    /// The new current version, with the restored version's contents.
    version: VersionView,
    /// The version that was current before, which **Undo** restores.
    previous: VersionId,
}

/// `POST /documents/{id}/versions/{version}/restore`: makes a version
/// current again, as a new version, after saving typing. Everyone with the
/// document open sees it change, since core takes in the file as a change
/// from outside the editor.
pub(super) async fn restore(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path((id, version)): Path<(ItemId, VersionId)>,
) -> Result<Json<Restored>, ApiError> {
    editing(&state, &me, id).await?;
    state.coediting.save_now(id).await;
    let previous = newest(&state, id).await?;
    // The current version is already current.
    if previous.id != version {
        state
            .storage
            .acting_as(accordion_module::auth::actor(&me))
            .restore_version(id, version)
            .await?;
    }
    let current = newest(&state, id).await?;
    let mut views = views(&state, vec![current]).await?;
    Ok(Json(Restored {
        version: views.remove(0),
        previous: previous.id,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VersionCopy {
    /// The copy's name, which gets `.html` if it doesn't end with it, and a
    /// number if it's taken.
    name: String,
}

/// `POST /documents/{id}/versions/{version}/copy`: a new document from a
/// version, beside the original if the person can add there, else in their
/// Docs place. It has no comments, since comments belong to the document,
/// not to its versions.
pub(super) async fn copy(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path((id, version)): Path<(ItemId, VersionId)>,
    Json(request): Json<VersionCopy>,
) -> Result<(StatusCode, Json<DocumentView>), ApiError> {
    let item = editing(&state, &me, id).await?;
    let stem = copy_stem(&request.name)?;
    let contents = contents_of(&state, id, version).await?;
    let file = off_thread(move || format::file_from_version(&contents)).await?;
    make_copy(&state, &me, &item, stem, Some(file)).await
}
