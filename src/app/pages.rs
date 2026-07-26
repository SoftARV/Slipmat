// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The navigation stack: pushing an album or artist page, and taking it away.
//!
//! Pages are addressed by **id**, never by their depth in the stack. By the
//! time a fetch lands or a click arrives the stack may have moved, and an index
//! that was right when the page was built is a wrong answer that looks like a
//! right one — the same lesson `queue.rs` is built around.

use relm4::ComponentSender;

use super::{ART_SIZE, AppModel, AppMsg, CommandMsg, DetailPage, PageKind, RowState, artwork};
use crate::music::client::Client;
use crate::music::types::Artwork;

impl AppModel {
    /// Push an album or artist page and ask Apple to fill it.
    ///
    /// The page appears immediately with a spinner rather than after the
    /// request lands: a click that does nothing for a second reads as a click
    /// that did not register, and the second click pushes it twice.
    pub(super) fn push_page(&mut self, kind: PageKind, sender: &ComponentSender<Self>) {
        let Some(tokens) = &self.tokens else {
            self.toast("Not connected yet");
            return;
        };
        let client = Client::new(
            tokens.developer_token.clone(),
            tokens.music_user_token.clone(),
            tokens.storefront.clone(),
        );

        let id = self.next_page_id;
        self.next_page_id += 1;

        let activate = sender.clone();
        let play = sender.clone();
        let shuffle = sender.clone();
        let page = DetailPage::new(
            id,
            kind.heading(),
            RowState {
                current: self.current_track.clone(),
                dead: self.dead_rows.clone(),
            },
            move |row| activate.input(AppMsg::DetailActivated { page: id, row }),
            move || {
                play.input(AppMsg::PlayPage {
                    page: id,
                    shuffle: false,
                })
            },
            move || {
                shuffle.input(AppMsg::PlayPage {
                    page: id,
                    shuffle: true,
                })
            },
        );
        page.set_end_controls(!self.show_queue);
        self.nav.push(page.widget());
        self.pages.push(page);

        let catalog_id = kind.id().to_owned();
        tracing::info!(page = id, kind = ?kind, "opening page");
        match kind {
            PageKind::Album(_) => sender.oneshot_command(async move {
                CommandMsg::AlbumPage {
                    page: id,
                    result: client
                        .album(&catalog_id)
                        .await
                        .map_err(|err| format!("{err:#}")),
                }
            }),
            PageKind::LibraryAlbum(_) => sender.oneshot_command(async move {
                CommandMsg::AlbumPage {
                    page: id,
                    result: client
                        .library_album(&catalog_id)
                        .await
                        .map_err(|err| format!("{err:#}")),
                }
            }),
            PageKind::Artist(_) => sender.oneshot_command(async move {
                CommandMsg::ArtistPage {
                    page: id,
                    result: client
                        .artist_albums(&catalog_id)
                        .await
                        .map_err(|err| format!("{err:#}")),
                }
            }),
            PageKind::LibraryPlaylist(_) => sender.oneshot_command(async move {
                CommandMsg::PlaylistPage {
                    page: id,
                    result: client
                        .library_playlist(&catalog_id)
                        .await
                        .map_err(|err| format!("{err:#}")),
                }
            }),
            PageKind::LibraryArtist(_) => sender.oneshot_command(async move {
                CommandMsg::ArtistPage {
                    page: id,
                    result: client
                        .library_artist_albums(&catalog_id)
                        .await
                        .map_err(|err| format!("{err:#}")),
                }
            }),
        }
    }

    /// Return to the results list, dropping whatever was pushed over it.
    ///
    /// Cheap when there is nothing to do, so callers do not have to check.
    pub(super) fn pop_to_results(&mut self) {
        if self.pages.is_empty() {
            return;
        }
        tracing::debug!(depth = self.pages.len(), "returning to results");
        // `connect_popped` fires per page and empties `self.pages` for us —
        // clearing it here as well would be a second source of truth for the
        // same thing.
        self.nav.pop_to_tag("results");
    }

    /// Keep the pushed pages' headers agreeing with the root about who owns the
    /// window controls. When the queue is open it is the rightmost pane, so the
    /// controls belong to its header — a page that still draws them puts a
    /// second close button in the middle of the window.
    pub(super) fn sync_page_controls(&self) {
        for page in &self.pages {
            page.set_end_controls(!self.show_queue);
        }
    }

    /// Fetch a page's header art, once we know what it is.
    pub(super) fn fetch_page_art(
        &self,
        page: u64,
        art: Option<Artwork>,
        sender: &ComponentSender<Self>,
    ) {
        let Some(art) = art else { return };
        sender.oneshot_command(async move {
            // A missing cover is cosmetic — `None` and the page keeps its
            // placeholder, exactly as the Now Playing bar does.
            CommandMsg::PageArtwork {
                page,
                path: artwork::fetch(art, ART_SIZE).await.ok(),
            }
        });
    }
}
