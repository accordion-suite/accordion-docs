//! Accordion Docs: documents edited together, live, as in Google Docs
//! (decision 30 in the `accordion` repo's `docs/decisions`).
//!
//! Each document is an HTML file that any browser opens, kept in its
//! owner's Docs place, which Drive shows as a tab beside My Drive
//! (decision 32). The file carries its own co-editing state in a data
//! block, so a document copied, synced, or restored from a backup still
//! opens in Docs with its history of edits. Core's co-editing service runs
//! the editing, and this crate gives it the format.

mod api;
mod format;
pub mod html;
pub mod model;
pub mod yjs;

use accordion_core::coedit::CoEditing;
use accordion_core::identity::Identity;
use accordion_core::storage::{AppPlace, Storage};
use accordion_core::thumbnails::Thumbnails;
use accordion_module::{BoxFuture, Module, Routes, Services, StartError, Started, Stopping};
use axum::extract::FromRef;

/// Docs' place beside My Drive.
pub(crate) const PLACE: AppPlace = AppPlace {
    id: "docs",
    name: "Docs",
};

/// The Docs module. Add it to the server's list of modules.
pub struct Docs;

impl Module for Docs {
    fn id(&self) -> &'static str {
        "docs"
    }

    fn name(&self) -> &'static str {
        "Docs"
    }

    /// Documents are files in Drive.
    fn requires(&self) -> &'static [&'static str] {
        &["drive"]
    }

    fn start(
        &self,
        services: Services,
        _stopping: Stopping,
    ) -> BoxFuture<Result<Started, StartError>> {
        Box::pin(async move {
            services.storage.add_place(PLACE);
            let state = DocsState {
                identity: services.identity,
                storage: services.storage,
                coediting: services.coediting,
                thumbnails: services.thumbnails,
            };
            Ok(Started {
                routes: Routes {
                    api: Some(api::routes(state)),
                    ..Routes::default()
                },
                background: None,
            })
        })
    }
}

#[derive(Clone)]
struct DocsState {
    identity: Identity,
    storage: Storage,
    coediting: CoEditing,
    thumbnails: Thumbnails,
}

impl FromRef<DocsState> for Identity {
    fn from_ref(state: &DocsState) -> Self {
        state.identity.clone()
    }
}
