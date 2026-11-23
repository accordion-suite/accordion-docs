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
pub mod docx;
pub mod fonts;
mod format;
pub mod html;
mod images;
pub mod import;
pub mod markdown;
pub mod model;
pub mod odt;
mod office;
pub mod picture;
pub mod style;
pub mod yjs;

use accordion_core::coedit::CoEditing;
use accordion_core::identity::Identity;
use accordion_core::storage::{AppPlace, Storage};
use accordion_core::thumbnails::Thumbnails;
use accordion_module::{BoxFuture, Module, Routes, Services, StartError, Started, Stopping};
use axum::extract::FromRef;

use crate::images::Images;

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
        stopping: Stopping,
    ) -> BoxFuture<Result<Started, StartError>> {
        Box::pin(async move {
            services.storage.add_place(PLACE);
            let images = Images::open(services.db).await?;
            let state = DocsState {
                identity: services.identity,
                storage: services.storage,
                coediting: services.coediting,
                thumbnails: services.thumbnails,
                images: images.clone(),
            };
            Ok(Started {
                routes: Routes {
                    api: Some(api::routes(state)),
                    ..Routes::default()
                },
                background: Some(Box::pin(tidy(images, stopping))),
            })
        })
    }
}

/// Once a day, from a minute after starting, leaves out pictures no
/// document has used for months.
async fn tidy(images: Images, stopping: Stopping) {
    let work = async {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        loop {
            match images.tidy().await {
                Ok(0) => {}
                Ok(removed) => tracing::info!(removed, "left out pictures no document uses"),
                Err(err) => tracing::warn!(%err, "couldn't tidy documents' pictures"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(24 * 60 * 60)).await;
        }
    };
    tokio::select! {
        () = work => {}
        () = stopping.wait() => {}
    }
}

#[derive(Clone)]
struct DocsState {
    identity: Identity,
    storage: Storage,
    coediting: CoEditing,
    thumbnails: Thumbnails,
    /// Pictures in documents, while they're open.
    images: Images,
}

impl FromRef<DocsState> for Identity {
    fn from_ref(state: &DocsState) -> Self {
        state.identity.clone()
    }
}
