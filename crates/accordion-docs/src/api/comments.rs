//! Comments on a document, at `/documents/{id}/comments` (decision 30,
//! step 6). They go through ordinary requests rather than the document's
//! WebSocket, so commenting never needs the right to edit. Each change
//! answers with every thread, sends a live hint to everyone who can open
//! the document, and writes the threads into the file.

use std::collections::{HashMap, HashSet};

use accordion_core::events::{Action, NewEvent};
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
    self, Comment, MAX_COMMENTS, MAX_MENTIONS, MAX_QUOTE, MAX_TEXT, MAX_THREADS, Mark, Mention,
    Thread,
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
    /// The people it mentions, each as `@` and their name in the text.
    mentions: Vec<Mention>,
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
                mentions: comment.mentions.clone(),
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
async fn ready(state: &DocsState, me: &SignedIn, id: ItemId) -> Result<(Item, Access), ApiError> {
    let (item, access) = reach(state, me, id).await?;
    if !can_comment(access) {
        return Err(cant_comment());
    }
    threads_of(state, &item).await?;
    Ok((item, access))
}

/// Someone a comment can mention.
#[derive(Serialize)]
pub(super) struct PersonView {
    id: UserId,
    name: String,
    username: String,
}

/// `GET /documents/{id}/people`: everyone else who can open the document,
/// by name, for whoever can comment on it. Only they can be mentioned, as
/// decision 30 says.
pub(super) async fn people(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path(id): Path<ItemId>,
) -> Result<Json<Vec<PersonView>>, ApiError> {
    let (_, access) = reach(&state, &me, id).await?;
    if !can_comment(access) {
        return Err(cant_comment());
    }
    let members = state.identity.members().await.map_err(|err| {
        tracing::error!("couldn't list people to mention: {err}");
        ApiError::server_problem()
    })?;
    let mut people = Vec::new();
    for member in members {
        if member.turned_off || member.user.id == me.user.id {
            continue;
        }
        if state.storage.access(member.user.id, id).await?.is_some() {
            people.push(PersonView {
                id: member.user.id,
                name: member.user.display_name,
                username: member.user.username,
            });
        }
    }
    people.sort_by_cached_key(|person| person.name.to_lowercase());
    Ok(Json(people))
}

/// The people a comment mentions, from those the person chose in the
/// editor: one the text no longer names with `@` was taken out again, and
/// everyone else must be able to open the document.
async fn mentions_in(
    state: &DocsState,
    id: ItemId,
    text: &str,
    chosen: Vec<UserId>,
) -> Result<Vec<Mention>, ApiError> {
    let mut seen = HashSet::new();
    let mut mentions = Vec::new();
    for user in chosen {
        if !seen.insert(user) {
            continue;
        }
        let found = state.identity.user(user).await.map_err(|err| {
            tracing::error!("couldn't look up someone mentioned: {err}");
            ApiError::server_problem()
        })?;
        let Some(found) = found else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "Someone you mentioned no longer has an account here. Take out their name, then try again.",
            ));
        };
        if !text.contains(&format!("@{}", found.display_name)) {
            continue;
        }
        if state.storage.access(user, id).await?.is_none() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!(
                    "{} can't open this document, so they can't be mentioned. Share it with them first, then try again.",
                    found.display_name
                ),
            ));
        }
        if mentions.len() == MAX_MENTIONS {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "A comment can mention at most 50 people. Take some out, then try again.",
            ));
        }
        mentions.push(Mention {
            user,
            name: found.display_name,
        });
    }
    Ok(mentions)
}

/// Rings the bells of the people a new or changed comment is for: those
/// newly `mentioned`, and for a reply, everyone else who wrote in the
/// thread. Only people who can still open the document hear about it, and
/// never the person who wrote it.
async fn tell(
    state: &DocsState,
    me: &SignedIn,
    item: &Item,
    thread: &Thread,
    comment: &Comment,
    mentioned: &[UserId],
    reply: bool,
) {
    let mut told = HashSet::from([me.user.id]);
    let mut people: Vec<(UserId, Action)> = Vec::new();
    for &user in mentioned {
        if told.insert(user) {
            people.push((user, Action::Mentioned));
        }
    }
    if reply {
        for earlier in &thread.comments {
            if let Some(author) = earlier.author
                && told.insert(author)
            {
                people.push((author, Action::Replied));
            }
        }
    }
    let details = serde_json::json!({
        "thread": thread.id,
        "comment": comment.id,
        "text": comments::shortened(&comment.text, MAX_QUOTE),
    });
    for (user, action) in people {
        match state.storage.access(user, item.id).await {
            Ok(Some(_)) => {}
            Ok(None) => continue,
            Err(err) => {
                tracing::warn!(%err, "couldn't check who can hear about a comment");
                continue;
            }
        }
        let event = NewEvent::about(action, item.id, item.owner)
            .other(user)
            .details(details.clone());
        if let Err(err) = state
            .events
            .record(accordion_module::auth::actor(me), event)
            .await
        {
            tracing::warn!(%err, "couldn't tell someone about a comment");
        }
    }
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

fn new_comment(me: &SignedIn, text: String, mentions: Vec<Mention>) -> Comment {
    Comment {
        id: comments::new_id(),
        author: Some(me.user.id),
        name: me.user.display_name.clone(),
        text,
        at: comments::now(),
        edited: None,
        mentions,
    }
}

/// The accounts of the people `mentions` names.
fn users(mentions: &[Mention]) -> Vec<UserId> {
    mentions.iter().map(|mention| mention.user).collect()
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
    /// The people the person chose to mention.
    #[serde(default)]
    mentions: Vec<UserId>,
}

/// `POST /documents/{id}/comments`: a new thread on some text.
pub(super) async fn start(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path(id): Path<ItemId>,
    Json(request): Json<NewThread>,
) -> Result<(StatusCode, Json<CommentsView>), ApiError> {
    let (item, access) = ready(&state, &me, id).await?;
    let text = comment_text(&request.text)?;
    let mentions = mentions_in(&state, id, &text, request.mentions).await?;
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
        comments: vec![new_comment(&me, text, mentions)],
    };
    let started = thread.clone();
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
    let first = &started.comments[0];
    tell(
        &state,
        &me,
        &item,
        &started,
        first,
        &users(&first.mentions),
        false,
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(changed(&state, &me, id, access, threads).await),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Text {
    text: String,
    /// The people the person chose to mention.
    #[serde(default)]
    mentions: Vec<UserId>,
}

/// `POST /documents/{id}/comments/{thread}`: a reply. Replying to a
/// resolved thread opens it again, as in Google Docs.
pub(super) async fn reply(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path((id, thread)): Path<(ItemId, String)>,
    Json(request): Json<Text>,
) -> Result<(StatusCode, Json<CommentsView>), ApiError> {
    let (item, access) = ready(&state, &me, id).await?;
    let text = comment_text(&request.text)?;
    let mentions = mentions_in(&state, id, &text, request.mentions).await?;
    let comment = new_comment(&me, text, mentions);
    let added = comment.clone();
    let (threads, before) = state
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
            let before = found.clone();
            found.comments.push(comment);
            found.resolved = None;
            Ok((threads.clone(), before))
        })
        .await?;
    tell(
        &state,
        &me,
        &item,
        &before,
        &added,
        &users(&added.mentions),
        true,
    )
    .await;
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
    let (_, access) = ready(&state, &me, id).await?;
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
    let (_, access) = ready(&state, &me, id).await?;
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
    let (item, access) = ready(&state, &me, id).await?;
    let text = comment_text(&request.text)?;
    let mentions = mentions_in(&state, id, &text, request.mentions).await?;
    let user = me.user.id;
    let comment_id = comment.clone();
    let (threads, changed_thread, before) = state
        .comments
        .change(id, move |threads| {
            let found_thread = threads
                .iter_mut()
                .find(|each| each.id == thread)
                .ok_or_else(thread_gone)?;
            let found = found_thread
                .comments
                .iter_mut()
                .find(|each| each.id == comment)
                .ok_or_else(thread_gone)?;
            if found.author != Some(user) {
                return Err(not_yours());
            }
            let before = users(&found.mentions);
            if found.text != text {
                found.text = text;
                found.edited = Some(comments::now());
            }
            found.mentions = mentions;
            let changed_thread = found_thread.clone();
            Ok((threads.clone(), changed_thread, before))
        })
        .await?;
    // Only people the change newly mentions hear about it.
    if let Some(found) = changed_thread
        .comments
        .iter()
        .find(|each| each.id == comment_id)
    {
        let new: Vec<UserId> = users(&found.mentions)
            .into_iter()
            .filter(|user| !before.contains(user))
            .collect();
        tell(&state, &me, &item, &changed_thread, found, &new, false).await;
    }
    Ok(Json(changed(&state, &me, id, access, threads).await))
}

/// `DELETE /documents/{id}/comments/{thread}/{comment}`: deletes a reply.
/// Deleting a thread's first comment deletes the thread, as in Google Docs.
pub(super) async fn remove(
    State(state): State<DocsState>,
    Authenticated(me): Authenticated,
    Path((id, thread, comment)): Path<(ItemId, String, String)>,
) -> Result<Json<CommentsView>, ApiError> {
    let (_, access) = ready(&state, &me, id).await?;
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
