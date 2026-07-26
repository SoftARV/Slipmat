// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Thin async wrapper over `api.music.apple.com`.
//!
//! Both tokens come from the sidecar (CLAUDE.md rule 7) — the developer token
//! as a bearer, the Music User Token in `Music-User-Token`. Neither is ever
//! logged or persisted to the config file.
//!
//! M5 fills in the library and catalog calls. What exists now is the client
//! itself plus the error diagnosis, because "errors name the fix" is easier to
//! honour if it's there from the first request rather than retrofitted.

use anyhow::{Context, Result};
use reqwest::{Client as HttpClient, StatusCode};
use serde::Deserialize;

use super::types::{
    Album, AlbumAttributes, AlbumResource, Artist, ArtistAttributes, ArtistResource,
    LibraryArtistResource, Playlist, PlaylistAttributes, Resource, Response, SongAttributes, Track,
};

/// What a catalog search turned up. Apple returns each kind in its own array;
/// this keeps them apart rather than flattening, because the UI shows them as
/// different things.
#[derive(Debug, Default)]
pub struct SearchResults {
    pub songs: Vec<Track>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
}

const API_BASE: &str = "https://api.music.apple.com/v1";
/// The origin the harvested developer token is minted for — see `get()`.
const WEB_ORIGIN: &str = "https://music.apple.com";
const WEB_REFERER: &str = "https://music.apple.com/";
/// A playlist long enough to hit this is a playlist nobody scrolls. Bounded so
/// one enormous one cannot spin through fifty requests.
const PLAYLIST_MAX: usize = 1_000;

/// Apple's hard cap for a library page. Asking for more is silently clamped.
const LIBRARY_PAGE: usize = 100;
/// How many library pages to fetch at once. The pages are independent, so
/// fetching them serially spent most of the load waiting on round trips.
const LIBRARY_CONCURRENCY: usize = 6;

/// Clone is cheap: `reqwest::Client` is an `Arc` internally, and the tokens are
/// small strings. Cloning per concurrent page keeps the connection pool shared.
#[derive(Clone)]
pub struct Client {
    http: HttpClient,
    developer_token: String,
    music_user_token: Option<String>,
    storefront: String,
}

/// Failures the UI has a distinct response to. Anything else is a toast.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("not signed in to Apple Music")]
    Unauthorized,
    /// 401 while holding a live user token — the request was rejected, not the
    /// session. In practice: a missing/wrong `Origin`, or a rotated token.
    #[error("Apple Music rejected the request (401) despite a valid session")]
    Rejected,
    #[error("no active Apple Music subscription")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("offline — check your connection")]
    Offline,
    #[error("Apple Music error ({0})")]
    Other(StatusCode),
}

impl Client {
    pub fn new(
        developer_token: String,
        music_user_token: Option<String>,
        storefront: String,
    ) -> Self {
        Self {
            http: HttpClient::new(),
            developer_token,
            music_user_token,
            storefront,
        }
    }

    pub fn storefront(&self) -> &str {
        &self.storefront
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        let req = self
            .http
            .get(format!("{API_BASE}{path}"))
            .bearer_auth(&self.developer_token)
            // The harvested developer token is ORIGIN-LOCKED. Its JWT payload
            // carries `"root_https_origin": ["apple.com"]`, and the API
            // enforces it: without these two headers every request comes back
            // 401 even with a perfectly valid token and user token. A browser
            // sets them automatically, which is why this only bites a native
            // client. Do not remove them.
            .header("Origin", WEB_ORIGIN)
            .header("Referer", WEB_REFERER);
        match &self.music_user_token {
            Some(t) => req.header("Music-User-Token", t.as_str()),
            None => req,
        }
    }

    /// Map a response status to something the UI can act on.
    ///
    /// `signed_in` matters: a 401 while holding a live user token is not a
    /// sign-in problem, it is a rejected *request*. Telling someone to sign in
    /// again when they already are sends them in circles — which is exactly
    /// what the first version of this did.
    fn diagnose(status: StatusCode, signed_in: bool) -> ApiError {
        match status {
            StatusCode::UNAUTHORIZED if signed_in => ApiError::Rejected,
            StatusCode::UNAUTHORIZED => ApiError::Unauthorized,
            StatusCode::FORBIDDEN => ApiError::Forbidden,
            StatusCode::NOT_FOUND => ApiError::NotFound,
            other => ApiError::Other(other),
        }
    }

    fn signed_in(&self) -> bool {
        self.music_user_token.is_some()
    }

    /// Turn a failed response into an error that carries Apple's own words.
    ///
    /// Apple returns a JSON body like
    /// `{"errors":[{"title":"Unauthorized","detail":"…"}]}`, and that detail is
    /// usually the difference between an hour of guessing and a one-line fix.
    /// The status alone is not enough — a bare 401 was what sent us chasing a
    /// sign-in problem that did not exist.
    async fn explain(&self, res: reqwest::Response) -> anyhow::Error {
        let status = res.status();
        let err = Self::diagnose(status, self.signed_in());
        match res.text().await {
            Ok(body) if !body.trim().is_empty() => {
                let mut detail = body.trim().replace('\n', " ");
                detail.truncate(400);
                tracing::warn!(%status, %detail, "apple music api error");
                anyhow::anyhow!("{err} — {detail}")
            }
            _ => {
                tracing::warn!(%status, "apple music api error with no body");
                err.into()
            }
        }
    }

    /// The user's whole saved-songs library.
    ///
    /// Apple caps a page at 100 and returns a `next` cursor, so this walks
    /// until the cursor runs out. `max` bounds it so a very large library
    /// cannot spin forever on a first run — the count is reported so the UI can
    /// say the list is partial rather than quietly truncating it.
    /// Fetches in **batches of concurrent pages** rather than one at a time.
    ///
    /// The pages are independent, so serial fetching spent the whole load
    /// waiting on round trips — six sequential requests for a 539-track
    /// library. Each round now issues `LIBRARY_CONCURRENCY` requests at once and
    /// stops as soon as a round comes back short, which is the same termination
    /// condition with far fewer waits.
    pub async fn all_library_songs(&self, max: usize) -> Result<Vec<Track>> {
        self.all_library::<Resource<SongAttributes>, Track>("songs", "", max)
            .await
    }

    /// Every album in the user's library.
    pub async fn all_library_albums(&self, max: usize) -> Result<Vec<Album>> {
        let mut albums = self
            .all_library::<Resource<AlbumAttributes>, Album>("albums", "", max)
            .await?;
        // Marked here rather than guessed from the id's shape later: these ids
        // only work against `/me/library/albums`, and the page that opens one
        // has to know which endpoint to ask.
        for album in &mut albums {
            album.library = true;
        }
        Ok(albums)
    }

    /// Every artist in the user's library.
    ///
    /// `include=catalog` is what gets the pictures. A library artist carries
    /// only a name — no artwork, no genres; the portrait the web player shows
    /// belongs to the *catalog* artist, and asking for it as a relationship
    /// costs no extra requests. If Apple ever stops honouring it the
    /// relationship comes back absent and the grid falls back to avatar
    /// placeholders, which is no worse than not having asked.
    pub async fn all_library_artists(&self, max: usize) -> Result<Vec<Artist>> {
        // `From<LibraryArtistResource>` already marks these as library-owned.
        self.all_library::<LibraryArtistResource, Artist>("artists", "&include=catalog", max)
            .await
    }

    /// Every playlist in the user's library.
    pub async fn all_library_playlists(&self, max: usize) -> Result<Vec<Playlist>> {
        let mut playlists = self
            .all_library::<Resource<PlaylistAttributes>, Playlist>("playlists", "", max)
            .await?;
        for playlist in &mut playlists {
            playlist.library = true;
        }
        Ok(playlists)
    }

    /// One library playlist, with its tracks.
    ///
    /// Two requests, unlike [`Client::library_album`]: `include=tracks` caps at
    /// 100 and playlists routinely run longer, so the tracks come from the
    /// relationship endpoint through the ordinary paginator. Silently showing
    /// the first 100 of a 400-track playlist is the kind of wrong answer that
    /// looks right.
    pub async fn library_playlist(&self, id: &str) -> Result<(Playlist, Vec<Track>)> {
        // Sequential rather than joined: the details request is one small
        // response, and the track walk below is already six pages at a time.
        // Overlapping them would save a round trip and cost a tokio feature.
        let playlist = self.library_playlist_details(id).await?;
        let tracks = self
            .all_library::<Resource<SongAttributes>, Track>(
                &format!("playlists/{id}/tracks"),
                "",
                PLAYLIST_MAX,
            )
            .await?;
        Ok((playlist, tracks))
    }

    async fn library_playlist_details(&self, id: &str) -> Result<Playlist> {
        let res = self
            .get(&format!("/me/library/playlists/{id}"))
            .send()
            .await
            .map_err(Self::transport_error)
            .context("requesting playlist")?;

        if !res.status().is_success() {
            return Err(self.explain(res).await);
        }

        let parsed: Response<Resource<PlaylistAttributes>> =
            res.json().await.context("decoding playlist")?;
        let mut playlist: Playlist = parsed
            .data
            .into_iter()
            .next()
            .context("playlist not found")?
            .into();
        playlist.library = true;
        Ok(playlist)
    }

    /// Walk a `/me/library/{kind}` collection to its end, or to `max`.
    ///
    /// Generic over Apple's wire shape `A` and our type `T` because songs,
    /// albums and artists paginate identically — three copies of this loop
    /// would drift, and the concurrency and short-page logic below is the
    /// fiddly part worth having once.
    /// `kind` is interpolated straight into `/me/library/{kind}`, so it can be
    /// a collection (`songs`) or a relationship (`playlists/p.123/tracks`).
    /// That is what gives playlist tracks real pagination rather than the 100
    /// that `include=tracks` caps out at.
    async fn all_library<R, T>(
        &self,
        kind: &str,
        extra_query: &'static str,
        max: usize,
    ) -> Result<Vec<T>>
    where
        R: serde::de::DeserializeOwned + Send + 'static,
        T: From<R> + Send + 'static,
    {
        let mut all: Vec<T> = Vec::new();
        let mut offset = 0usize;

        while all.len() < max {
            let offsets: Vec<usize> = (0..LIBRARY_CONCURRENCY)
                .map(|i| offset + i * LIBRARY_PAGE)
                .collect();

            let mut tasks = tokio::task::JoinSet::new();
            for (slot, at) in offsets.iter().copied().enumerate() {
                let client = self.clone();
                // Cloned per task: `kind` may be a borrowed path built from an
                // id, and the tasks outlive this loop iteration.
                let kind = kind.to_owned();
                tasks.spawn(async move {
                    (
                        slot,
                        client.library_page::<R, T>(&kind, extra_query, at).await,
                    )
                });
            }

            // Collect by slot so the library keeps Apple's ordering regardless
            // of which request finishes first.
            let mut pages: Vec<Option<Vec<T>>> = (0..LIBRARY_CONCURRENCY).map(|_| None).collect();
            while let Some(joined) = tasks.join_next().await {
                let (slot, page) = joined.context("library page task panicked")?;
                pages[slot] = Some(page?);
            }

            let mut round = 0usize;
            let mut short = false;
            for page in pages.into_iter().flatten() {
                round += page.len();
                if page.len() < LIBRARY_PAGE {
                    short = true;
                }
                all.extend(page);
            }

            // A short page anywhere in the round means we have reached the end.
            if short || round == 0 {
                break;
            }
            offset += round;
        }

        all.truncate(max);
        Ok(all)
    }

    async fn library_page<R, T>(
        &self,
        kind: &str,
        extra_query: &str,
        offset: usize,
    ) -> Result<Vec<T>>
    where
        R: serde::de::DeserializeOwned,
        T: From<R>,
    {
        let res = self
            .get(&format!(
                "/me/library/{kind}?limit={LIBRARY_PAGE}&offset={offset}{extra_query}"
            ))
            .send()
            .await
            .map_err(Self::transport_error)
            .with_context(|| format!("requesting library {kind}"))?;

        if !res.status().is_success() {
            return Err(self.explain(res).await);
        }

        let parsed: Response<R> = res
            .json()
            .await
            .with_context(|| format!("decoding library {kind}"))?;
        Ok(parsed.data.into_iter().map(T::from).collect())
    }

    /// Catalog search. Needs only the developer token — no user token, no
    /// subscription — which makes it the cheapest way to prove the harvested
    /// token actually works before any playback is involved.
    /// `offset` walks further into the results. Apple caps `limit` at 25 per
    /// request for search, so anything past the first 25 needs paging.
    pub async fn search(&self, term: &str, limit: u32, offset: usize) -> Result<SearchResults> {
        let query = urlencode(term);
        let res = self
            .get(&format!(
                "/catalog/{}/search?types=songs,albums,artists&limit={limit}&offset={offset}&term={query}",
                self.storefront
            ))
            .send()
            .await
            .map_err(|err| {
                if err.is_connect() {
                    ApiError::Offline
                } else {
                    ApiError::Other(StatusCode::BAD_GATEWAY)
                }
            })
            .context("searching the catalog")?;

        if !res.status().is_success() {
            return Err(self.explain(res).await);
        }

        // Search nests its payload differently to every other endpoint:
        // results -> songs -> data, and `songs` is absent (not empty) when
        // nothing matched.
        let parsed: SearchResponse = res.json().await.context("decoding search results")?;
        Ok(SearchResults {
            songs: parsed
                .results
                .songs
                .map(|s| s.data.into_iter().map(Track::from).collect())
                .unwrap_or_default(),
            albums: parsed
                .results
                .albums
                .map(|a| a.data.into_iter().map(Album::from).collect())
                .unwrap_or_default(),
            artists: parsed
                .results
                .artists
                .map(|a| a.data.into_iter().map(Artist::from).collect())
                .unwrap_or_default(),
        })
    }

    /// An album and its tracks, in one request.
    ///
    /// `include=tracks` saves a round trip: the relationship comes back inside
    /// the album resource rather than needing a second call.
    pub async fn album(&self, id: &str) -> Result<(Album, Vec<Track>)> {
        let resource = self
            .album_resource(&format!(
                "/catalog/{}/albums/{id}?include=tracks",
                self.storefront
            ))
            .await?;
        let tracks = album_tracks(&resource);
        Ok((Album::from(resource.into_album()), tracks))
    }

    /// Fetch one album resource, whichever collection it lives in. The catalog
    /// and library responses have the same shape; only the URL differs.
    async fn album_resource(&self, path: &str) -> Result<AlbumResource> {
        let res = self
            .get(path)
            .send()
            .await
            .map_err(Self::transport_error)
            .context("requesting album")?;

        if !res.status().is_success() {
            return Err(self.explain(res).await);
        }

        let parsed: Response<AlbumResource> = res.json().await.context("decoding album")?;
        parsed.data.into_iter().next().context("album not found")
    }

    /// One album from the user's library, with the tracks they saved.
    ///
    /// Distinct from [`Client::album`] only in the URL: library ids 404 against
    /// `/catalog`. The response shape is identical, so the parsing is shared.
    pub async fn library_album(&self, id: &str) -> Result<(Album, Vec<Track>)> {
        let resource = self
            .album_resource(&format!("/me/library/albums/{id}?include=tracks"))
            .await?;
        let tracks = album_tracks(&resource);
        let mut album = Album::from(resource.into_album());
        album.library = true;
        Ok((album, tracks))
    }

    /// One artist from the user's library, with the albums they saved.
    pub async fn library_artist_albums(&self, id: &str) -> Result<(Artist, Vec<Album>)> {
        // `catalog` alongside `albums`: the portrait belongs to the catalog
        // artist, exactly as in `all_library_artists`, so the page header
        // matches the tile that opened it.
        let resource = self
            .artist_resource(&format!("/me/library/artists/{id}?include=albums,catalog"))
            .await?;
        let portrait = resource
            .relationships
            .as_ref()
            .and_then(|r| r.catalog.as_ref())
            .and_then(|c| c.data.first())
            .cloned()
            .map(Artist::from);

        let mut albums = artist_albums_of(&resource);

        // `include=` is documented for catalog resources and merely *works* for
        // library ones. If Apple ever stops honouring it the relationship comes
        // back absent, which would render as an artist who owns no albums —
        // a silent wrong answer. Ask the relationship endpoint directly instead.
        if albums.is_empty() {
            tracing::debug!(%id, "library artist had no included albums; asking directly");
            albums = self
                .library_pageless_albums(&format!("/me/library/artists/{id}/albums"))
                .await
                .unwrap_or_default();
        }

        for album in &mut albums {
            album.library = true;
        }

        let mut artist = Artist::from(resource.into_artist());
        artist.library = true;
        if let Some(portrait) = portrait {
            artist.artwork = portrait.artwork;
            artist.genres = portrait.genres;
        }
        Ok((artist, albums))
    }

    /// A one-shot relationship fetch — no paging. Used only as the fallback
    /// above, where a first page of albums is better than none.
    async fn library_pageless_albums(&self, path: &str) -> Result<Vec<Album>> {
        let res = self
            .get(path)
            .send()
            .await
            .map_err(Self::transport_error)
            .context("requesting library artist albums")?;

        if !res.status().is_success() {
            return Err(self.explain(res).await);
        }

        let parsed: Response<Resource<AlbumAttributes>> =
            res.json().await.context("decoding library artist albums")?;
        Ok(parsed.data.into_iter().map(Album::from).collect())
    }

    /// An artist's albums, newest first as Apple orders them.
    pub async fn artist_albums(&self, id: &str) -> Result<(Artist, Vec<Album>)> {
        let resource = self
            .artist_resource(&format!(
                "/catalog/{}/artists/{id}?include=albums",
                self.storefront
            ))
            .await?;
        let albums = artist_albums_of(&resource);
        Ok((Artist::from(resource.into_artist()), albums))
    }

    /// As [`Client::album_resource`], for artists.
    async fn artist_resource(&self, path: &str) -> Result<ArtistResource> {
        let res = self
            .get(path)
            .send()
            .await
            .map_err(Self::transport_error)
            .context("requesting artist")?;

        if !res.status().is_success() {
            return Err(self.explain(res).await);
        }

        let parsed: Response<ArtistResource> = res.json().await.context("decoding artist")?;
        parsed.data.into_iter().next().context("artist not found")
    }

    fn transport_error(err: reqwest::Error) -> ApiError {
        if err.is_connect() {
            ApiError::Offline
        } else {
            ApiError::Other(StatusCode::BAD_GATEWAY)
        }
    }
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: SearchWire,
}

/// Apple omits a key entirely when a kind matched nothing, rather than sending
/// an empty array — so every field here is optional.
#[derive(Debug, Default, Deserialize)]
struct SearchWire {
    songs: Option<Response<Resource<SongAttributes>>>,
    albums: Option<Response<Resource<AlbumAttributes>>>,
    artists: Option<Response<Resource<ArtistAttributes>>>,
}

/// Percent-encode a search term. The full `url` crate is a lot of dependency
/// for one query parameter; this covers everything not unreserved per RFC 3986.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The tracks Apple attached to an album via `include=tracks`, or none.
fn album_tracks(resource: &AlbumResource) -> Vec<Track> {
    resource
        .relationships
        .as_ref()
        .and_then(|r| r.tracks.as_ref())
        .map(|t| t.data.iter().cloned().map(Track::from).collect())
        .unwrap_or_default()
}

/// The albums Apple attached to an artist via `include=albums`, or none.
fn artist_albums_of(resource: &ArtistResource) -> Vec<Album> {
    resource
        .relationships
        .as_ref()
        .and_then(|r| r.albums.as_ref())
        .map(|a| a.data.iter().cloned().map(Album::from).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_results_are_nested_differently_to_every_other_endpoint() {
        let raw = r#"{"results":{"songs":{"data":[
            {"id":"1440857781","attributes":{"name":"Roundabout","artistName":"Yes"}}]}}}"#;
        let parsed: SearchResponse = serde_json::from_str(raw).unwrap();
        let songs = parsed.results.songs.expect("songs present");
        assert_eq!(songs.data[0].id, "1440857781");
    }

    #[test]
    fn a_search_that_matches_nothing_omits_the_key_entirely() {
        // Apple drops `songs` rather than returning an empty array — treating
        // that as an error would make every no-results search look broken.
        let parsed: SearchResponse = serde_json::from_str(r#"{"results":{}}"#).unwrap();
        assert!(parsed.results.songs.is_none());
    }

    #[test]
    fn search_terms_are_percent_encoded() {
        assert_eq!(urlencode("Sigur Rós & co"), "Sigur%20R%C3%B3s%20%26%20co");
        assert_eq!(urlencode("plain-term_1.0~x"), "plain-term_1.0~x");
    }

    #[test]
    fn statuses_map_to_errors_that_name_the_fix() {
        assert!(matches!(
            Client::diagnose(StatusCode::UNAUTHORIZED, false),
            ApiError::Unauthorized
        ));
        assert!(
            Client::diagnose(StatusCode::FORBIDDEN, true)
                .to_string()
                .contains("subscription")
        );
    }

    #[test]
    fn a_401_while_signed_in_does_not_tell_you_to_sign_in() {
        // The original bug report: signed in with a live user token, and the
        // app said "sign in again" — sending you round in a circle when the
        // real cause was a rejected request.
        let err = Client::diagnose(StatusCode::UNAUTHORIZED, true);
        assert!(matches!(err, ApiError::Rejected));
        let msg = err.to_string();
        assert!(!msg.contains("sign in"), "misleading message: {msg}");
        assert!(msg.contains("valid session"));
    }
}
