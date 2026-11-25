//! Comments on a document, at `/documents/{id}/comments` (decision 30,
//! step 6). They go through ordinary requests rather than the document's
//! WebSocket, so commenting never needs the right to edit. Each change
//! answers with every thread, sends a live hint to everyone who can open
//! the document, and writes the threads into the file.

use std::collections::HashMap;

use accordion_core::identity::SignedIn;
use accordion_core::live::Hint;
use accordion_core::storage::{Access, Item};
use accordion_module::{ApiError, Authenticated};
use accordion_types::{ItemId, UserId};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use super::{reach, read_contents};
use crate::DocsState;
use crate::comments::{
    self, Comment, MAX_COMMENTS, MAX_QUOTE, MAX_TEXT, MAX_THREADS, Mark, Thread,
};
use crate::html;

/// The kind of live hint a change to a document's comments sends, naming
/// the document.
pub(crate) const HINT: &str = "docs.comments";

/// A document's threads, as the web app shows them.
#[derive(Serialize)]
pub(super) struct CommentsView {
    /// Whether the person can start threads, reply, and resolve them.
    can_comment: bool,
    /// Oldest first. The editor puts them in the order of their text.
    threads: Vec<ThreadView>,
}

#[derive(Serialize)]
struct ThreadView {
    id: String,
    /// Where the text starts and ends, as Yjs relative positions in base64.
    start: String,
    end: String,
    /// The text as it was when the thread started.
    quote: String,
    /// Who resolved it and when, while it's resolved.
    resolved: Option<MarkView>,
    /// Whether the person can delete the whole thread.
    can_delete: bool,
    comments: Vec<CommentView>,
}

#[derive(Serialize)]
struct MarkView {
    name: String,
    at: String,
}

#[derive(Serialize)]
struct CommentView {
    id: String,
    /// The author's name, as it is now if they're on this server.
    name: String,
    /// Whether the person wrote it, and so can change it.
    mine: bool,
    /// Whether the person can delete it.
    can_delete: bool,
    text: String,
    at: String,
    edited: Option<String>,
}

/// Whether someone with `access` can comment: anyone but a viewer.
/// Commenters can't change the text, but they start threads, reply,
/// resolve, and change their own comments, as in Google Docs.
fn can_comment(access: Access) -> bool {
    access >= Access::Commenter
}

/// Whether `user`, with `access`, can delete `comment`: its author can, and
/// so can anyone who can edit the document, as in Google Docs.
fn can_delete(comment: &Comment, user: UserId, access: Access) -> bool {
    comment.author == Some(user) || access >= Access::Editor
}

fn cant_comment() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "You can view this document but not comment on it. Ask its owner to let you comment.",
    )
}

fn thread_gone() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "That comment isn't there anymore. Someone may have deleted it.",
    )
}

/// The text of a new or changed comment, trimmed, or why it can't be one.
fn comment_text(text: &str) -> Result<String, ApiError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "Write a comment first, then try again.",
        ));
    }
    if text.chars().count() > MAX_TEXT {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "A comment can be at most 4,000 characters. Shorten it, then try again.",
        ));
    }
    Ok(text.to_owned())
}

/// The document's threads, taken in from its file first if Docs hasn't
/// read them yet.
pub(super) async fn threads_of(state: &DocsState, item: &Item) -> Result<Vec<Thread>, ApiError> {
    if let Some(threads) = state.comments.get(item.id).await.map_err(db_problem)? {
        return Ok(threads);
    }
    let threads = match &item.kind {
        accordion_core::storage::ItemKind::File { blob, .. } => {
            html::comments_in(&read_contents(state, blob.hash).await?)
        }
        accordion_core::storage::ItemKind::Folder => Vec::new(),
    };
    state
        .comments
        .take_in(item.id, threads)
        .await
        .map_err(db_problem)?;
    Ok(state
        .comments
        .get(item.id)
        .await
        .map_err(db_problem)?
        .unwrap_or_default())
}

fn db_problem(err: accordion_core::db::Error) -> ApiError {
    tracing::error!("couldn't read documents' comments: {err}");
    ApiError::server_problem()
}

/// People's names as they are now, for people on this server, looked up
/// once each.
#[derive(Default)]
struct Names(HashMap<UserId, Option<String>>);

impl Names {
    /// The name of `by`, or `kept`, the name the comment carries, for
    /// someone not on this server.
    async fn of(&mut self, state: &DocsState, by: Option<UserId>, kept: &str) -> String {
        let Some(by) = by else {
            return kept.to_owned();
        };
        let name = match self.0.get(&by) {
            Some(name) => name.clone(),
            None => {
                let found = state
                    .identity
                    .user(by)
                    .await
                    .ok()
                    .flatten()
                    .map(|found| found.display_name);
                self.0.insert(by, found.clone());
                found
            }
        };
        name.unwrap_or_else(|| kept.to_owned())
    }
}

/// Everything the web app shows of `threads` to `me`.
async fn view(
    state: &DocsState,
    me: &SignedIn,
    access: Access,
    threads: Vec<Thread>,
) -> CommentsView {
    let user = me.user.id;
    let mut names = Names::default();
    names.0.insert(user, Some(me.user.display_name.clone()));
    let mut views = Vec::with_capacity(threads.len());
    for thread in threads {
        let resolved = match &thread.resolved {
            Some(mark) => Some(MarkView {
                name: names.of(state, mark.by, &mark.name).await,
                at: mark.at.clone(),
            }),
            None => None,
        };
        let can_delete_thread = thread
            .comments
            .first()
            .is_some_and(|first| can_delete(first, user, access));
        let mut comments = Vec::with_capacity(thread.comments.len());
        for comment in &thread.comments {
            comments.push(CommentView {
                id: comment.id.clone(),
                name: names.of(state, comment.author, &comment.name).await,
                mine: comment.author == Some(user),
                can_delete: can_delete(comment, user, access),
                text: comment.text.clone(),
                at: comment.at.clone(),
                edited: comment.edited.clone(),
            });
        }
        views.push(ThreadView {
            id: thread.id,
            start: thread.start,
            end: thread.end,
            quote: thread.quote,
            resolved,
            can_delete: can_delete_thread,
            comments,
        });
    }
    CommentsView {
        can_comment: can_comment(access),
        threads: views,
    }
}

/// `GET /documents/{id}/comments`: the document's threads, for anyone who
/// can open it.
pub(super) async fn list(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path(id): Path<ItemId>,
) -> Result<Json<CommentsView>, ApiError> {
    let (item, access) = reach(&state, &me, id).await?;
    let threads = threads_of(&state, &item).await?;
    Ok(Json(view(&state, &me, access, threads).await))
}

/// Checks the person can comment on the document, and has its threads
/// taken in, so a change starts from them.
async fn ready(state: &DocsState, me: &SignedIn, id: ItemId) -> Result<Access, ApiError> {
    let (item, access) = reach(state, me, id).await?;
    if !can_comment(access) {
        return Err(cant_comment());
    }
    threads_of(state, &item).await?;
    Ok(access)
}

/// After a change: tells everyone who has the document open, writes the
/// threads into the file after any typing not saved yet, and answers with
/// them.
async fn changed(
    state: &DocsState,
    me: &SignedIn,
    id: ItemId,
    access: Access,
    threads: Vec<Thread>,
) -> CommentsView {
    state.live.send(Hint::item(HINT, id));
    // Typing not saved yet goes into the file first. Otherwise the
    // co-editing service could take the file the comments go into, from
    // before that typing, as the document, and the typing would be lost.
    state.coediting.save_now(id).await;
    state
        .comments
        .write_file(&state.storage, accordion_module::auth::actor(me), id)
        .await;
    view(state, me, access, threads).await
}

fn mark(me: &SignedIn) -> Mark {
    Mark {
        by: Some(me.user.id),
        name: me.user.display_name.clone(),
        at: comments::now(),
    }
}

fn new_comment(me: &SignedIn, text: String) -> Comment {
    Comment {
        id: comments::new_id(),
        author: Some(me.user.id),
        name: me.user.display_name.clone(),
        text,
        at: comments::now(),
        edited: None,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NewThread {
    /// Where the text starts and ends, as Yjs relative positions in base64.
    start: String,
    end: String,
    /// The text, to show beside the thread once it's gone.
    #[serde(default)]
    quote: String,
    text: String,
}

/// `POST /documents/{id}/comments`: a new thread on some text.
pub(super) async fn start(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path(id): Path<ItemId>,
    Json(request): Json<NewThread>,
) -> Result<(StatusCode, Json<CommentsView>), ApiError> {
    let access = ready(&state, &me, id).await?;
    let text = comment_text(&request.text)?;
    if !comments::is_anchor(&request.start) || !comments::is_anchor(&request.end) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "Docs couldn't tell which text this comment is about. Select the text again, then try again.",
        ));
    }
    let quote = comments::shortened(
        &request
            .quote
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        MAX_QUOTE,
    );
    let thread = Thread {
        id: comments::new_id(),
        start: request.start,
        end: request.end,
        quote,
        resolved: None,
        comments: vec![new_comment(&me, text)],
    };
    let threads = state
        .comments
        .change(id, move |threads| {
            if threads.len() >= MAX_THREADS {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "This document has as many comments as it can hold. Delete some, then try again.",
                ));
            }
            threads.push(thread);
            Ok(threads.clone())
        })
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(changed(&state, &me, id, access, threads).await),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Text {
    text: String,
}

/// `POST /documents/{id}/comments/{thread}`: a reply. Replying to a
/// resolved thread opens it again, as in Google Docs.
pub(super) async fn reply(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path((id, thread)): Path<(ItemId, String)>,
    Json(request): Json<Text>,
) -> Result<(StatusCode, Json<CommentsView>), ApiError> {
    let access = ready(&state, &me, id).await?;
    let comment = new_comment(&me, comment_text(&request.text)?);
    let threads = state
        .comments
        .change(id, move |threads| {
            let found = threads
                .iter_mut()
                .find(|each| each.id == thread)
                .ok_or_else(thread_gone)?;
            if found.comments.len() >= MAX_COMMENTS {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "This comment has as many replies as it can hold. Start a new comment instead.",
                ));
            }
            found.comments.push(comment);
            found.resolved = None;
            Ok(threads.clone())
        })
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(changed(&state, &me, id, access, threads).await),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Resolve {
    resolved: bool,
}

/// `PATCH /documents/{id}/comments/{thread}`: resolves a thread, which
/// hides it beside the text, or opens it again.
pub(super) async fn resolve(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path((id, thread)): Path<(ItemId, String)>,
    Json(request): Json<Resolve>,
) -> Result<Json<CommentsView>, ApiError> {
    let access = ready(&state, &me, id).await?;
    let resolved = request.resolved.then(|| mark(&me));
    let threads = state
        .comments
        .change(id, move |threads| {
            let found = threads
                .iter_mut()
                .find(|each| each.id == thread)
                .ok_or_else(thread_gone)?;
            if found.resolved.is_some() != resolved.is_some() {
                found.resolved = resolved;
            }
            Ok(threads.clone())
        })
        .await?;
    Ok(Json(changed(&state, &me, id, access, threads).await))
}

/// `DELETE /documents/{id}/comments/{thread}`: deletes a thread and its
/// replies, for whoever started it and anyone who can edit the document.
pub(super) async fn remove_thread(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path((id, thread)): Path<(ItemId, String)>,
) -> Result<Json<CommentsView>, ApiError> {
    let access = ready(&state, &me, id).await?;
    let user = me.user.id;
    let threads = state
        .comments
        .change(id, move |threads| {
            let at = threads
                .iter()
                .position(|each| each.id == thread)
                .ok_or_else(thread_gone)?;
            if !threads[at]
                .comments
                .first()
                .is_some_and(|first| can_delete(first, user, access))
            {
                return Err(not_yours());
            }
            threads.remove(at);
            Ok(threads.clone())
        })
        .await?;
    Ok(Json(changed(&state, &me, id, access, threads).await))
}

fn not_yours() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "Only the person who wrote this comment can change it.",
    )
}

/// `PATCH /documents/{id}/comments/{thread}/{comment}`: changes a comment's
/// text, for its author only.
pub(super) async fn edit(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path((id, thread, comment)): Path<(ItemId, String, String)>,
    Json(request): Json<Text>,
) -> Result<Json<CommentsView>, ApiError> {
    let access = ready(&state, &me, id).await?;
    let text = comment_text(&request.text)?;
    let user = me.user.id;
    let threads = state
        .comments
        .change(id, move |threads| {
            let found = threads
                .iter_mut()
                .find(|each| each.id == thread)
                .and_then(|thread| thread.comments.iter_mut().find(|each| each.id == comment))
                .ok_or_else(thread_gone)?;
            if found.author != Some(user) {
                return Err(not_yours());
            }
            if found.text != text {
                found.text = text;
                found.edited = Some(comments::now());
            }
            Ok(threads.clone())
        })
        .await?;
    Ok(Json(changed(&state, &me, id, access, threads).await))
}

/// `DELETE /documents/{id}/comments/{thread}/{comment}`: deletes a reply.
/// Deleting a thread's first comment deletes the thread, as in Google Docs.
pub(super) async fn remove(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path((id, thread, comment)): Path<(ItemId, String, String)>,
) -> Result<Json<CommentsView>, ApiError> {
    let access = ready(&state, &me, id).await?;
    let user = me.user.id;
    let threads = state
        .comments
        .change(id, move |threads| {
            let at = threads
                .iter()
                .position(|each| each.id == thread)
                .ok_or_else(thread_gone)?;
            let position = threads[at]
                .comments
                .iter()
                .position(|each| each.id == comment)
                .ok_or_else(thread_gone)?;
            if !can_delete(&threads[at].comments[position], user, access) {
                return Err(not_yours());
            }
            if position == 0 {
                threads.remove(at);
            } else {
                threads[at].comments.remove(position);
            }
            Ok(threads.clone())
        })
        .await?;
    Ok(Json(changed(&state, &me, id, access, threads).await))
}
