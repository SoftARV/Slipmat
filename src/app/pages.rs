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
use crate::music::types::{Artwork, Track};

/// How many covers a mosaic is made of. Apple's own is 2×2 and so is ours.
pub(super) const MOSAIC_TILES: usize = 4;

/// The first `want` **distinct** covers, in order.
///
/// Distinct by cache key, and that is the whole subtlety: a playlist drawn from
/// two albums would otherwise tile the same sleeve twice over, which reads as a
/// rendering fault rather than as a playlist.
///
/// Takes an iterator rather than `&[Track]` so it can be tested without
/// building a `Track`, which has no `Default` and a dozen fields.
pub(super) fn distinct_covers<'a>(
    arts: impl Iterator<Item = &'a Artwork>,
    want: usize,
) -> Vec<Artwork> {
    let mut seen = std::collections::HashSet::new();
    let mut covers = Vec::with_capacity(want);
    for art in arts {
        if seen.insert(art.cache_key()) {
            covers.push(art.clone());
            if covers.len() == want {
                break;
            }
        }
    }
    covers
}

/// The covers a playlist's mosaic would be built from.
///
/// Fewer than [`MOSAIC_TILES`] of them means no mosaic at all: a half-filled
/// grid looks broken in a way an empty sleeve does not.
pub(super) fn playlist_covers(tracks: &[Track]) -> Vec<Artwork> {
    distinct_covers(
        tracks.iter().filter_map(|track| track.artwork.as_ref()),
        MOSAIC_TILES,
    )
}

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
                overrides: self.row_overrides.clone(),
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
        page.set_end_controls(true);
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
            PageKind::Playlist(_) => sender.oneshot_command(async move {
                CommandMsg::PlaylistPage {
                    page: id,
                    result: client
                        .playlist(&catalog_id)
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
            page.set_end_controls(true);
        }
    }

    /// A playlist page's header picture: Apple's if there is one, ours if not.
    ///
    /// Only playlists reach the second branch. Albums and artists always have
    /// artwork of their own — an artist's comes from the catalog twin — but
    /// **Apple sends nothing for a playlist you made yourself**, which is why
    /// the app showed an empty sleeve where its own web player shows a mosaic.
    ///
    /// This is the one place Slipmat draws a picture Apple did not send, and it
    /// is deliberately the *cheap* one: a page has already loaded its tracks,
    /// so the covers are known and mostly cached. The grid cannot do this — a
    /// tile knows no tracks, so it would cost a request per artless playlist.
    pub(super) fn fetch_page_art_or_mosaic(
        &self,
        page: u64,
        art: Option<Artwork>,
        covers: Vec<Artwork>,
        sender: &ComponentSender<Self>,
    ) {
        if art.is_some() {
            self.fetch_page_art(page, art, sender);
            return;
        }
        if covers.len() < MOSAIC_TILES {
            tracing::warn!(
                page,
                have = covers.len(),
                "too few distinct covers for a mosaic; keeping the empty sleeve"
            );
            return;
        }
        sender.oneshot_command(async move {
            CommandMsg::PageArtwork {
                page,
                path: artwork::mosaic(covers, ART_SIZE).await,
            }
        });
    }

    /// Fetch a page's header art, once we know what it is.
    pub(super) fn fetch_page_art(
        &self,
        page: u64,
        art: Option<Artwork>,
        sender: &ComponentSender<Self>,
    ) {
        // **The quietest of the three exits, and the one that matters.** A page
        // whose resource carries no artwork at all keeps its placeholder having
        // asked for nothing — indistinguishable, from the outside, from a fetch
        // that failed or one still in flight. Note this is the artwork on the
        // *details* response, which is a different request from the one the
        // grid's `with_art` counter measures: a playlist can carry a picture in
        // one and not the other.
        let Some(art) = art else {
            tracing::warn!(page, "page resource carries no artwork; nothing to fetch");
            return;
        };
        sender.oneshot_command(async move {
            // A missing cover is cosmetic — `None` and the page keeps its
            // placeholder, exactly as the Now Playing bar does. **Cosmetic is
            // not the same as unexplained**, and this was `.ok()`: a header
            // that silently kept its placeholder was indistinguishable from a
            // playlist Apple has no picture for, which is exactly how a
            // playlist mosaic that used to render went missing without a word.
            // The error carries the URL it tried — see `artwork::fetch`.
            let path = match artwork::fetch(art, ART_SIZE).await {
                Ok(path) => Some(path),
                Err(err) => {
                    tracing::warn!(page, ?err, "page artwork not fetched");
                    None
                }
            };
            CommandMsg::PageArtwork { page, path }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn art(n: &str) -> Artwork {
        Artwork::new(format!("https://is1.mzstatic.com/{n}/{{w}}x{{h}}bb.jpg"))
    }

    #[test]
    fn a_mosaic_never_repeats_a_cover() {
        // The point of "distinct". A playlist drawn from two albums would
        // otherwise tile the same sleeve twice, which reads as a rendering
        // fault rather than as a playlist.
        let arts = [art("a"), art("a"), art("b"), art("a"), art("b")];
        let picked = distinct_covers(arts.iter(), MOSAIC_TILES);

        assert_eq!(picked.len(), 2, "two albums can only give two quadrants");
        assert_eq!(picked[0].cache_key(), art("a").cache_key());
        assert_eq!(picked[1].cache_key(), art("b").cache_key());
    }

    #[test]
    fn it_stops_at_four_however_long_the_playlist() {
        // 117 tracks was the real case; walking all of them to fill four slots
        // is work done for nothing.
        let arts: Vec<_> = (0..200).map(|i| art(&i.to_string())).collect();
        assert_eq!(
            distinct_covers(arts.iter(), MOSAIC_TILES).len(),
            MOSAIC_TILES
        );
    }

    #[test]
    fn order_follows_the_playlist() {
        // Top-left is the first track, so the mosaic matches what the list
        // underneath it shows.
        let arts = [art("z"), art("y"), art("x"), art("w")];
        let picked = distinct_covers(arts.iter(), MOSAIC_TILES);
        let keys: Vec<_> = picked.iter().map(Artwork::cache_key).collect();
        let want: Vec<_> = arts.iter().map(Artwork::cache_key).collect();
        assert_eq!(keys, want);
    }

    #[test]
    fn too_few_covers_yields_too_few() {
        // The caller keeps the empty sleeve rather than drawing a half grid.
        assert!(distinct_covers([art("a")].iter(), MOSAIC_TILES).len() < MOSAIC_TILES);
        assert!(distinct_covers([].iter(), MOSAIC_TILES).is_empty());
    }
}
