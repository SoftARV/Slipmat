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

use super::types::{Resource, Response, SongAttributes, Track};

const API_BASE: &str = "https://api.music.apple.com/v1";
/// The origin the harvested developer token is minted for — see `get()`.
const WEB_ORIGIN: &str = "https://music.apple.com";
const WEB_REFERER: &str = "https://music.apple.com/";
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
        let mut all: Vec<Track> = Vec::new();
        let mut offset = 0usize;

        while all.len() < max {
            let offsets: Vec<usize> = (0..LIBRARY_CONCURRENCY)
                .map(|i| offset + i * LIBRARY_PAGE)
                .collect();

            let mut tasks = tokio::task::JoinSet::new();
            for (slot, at) in offsets.iter().copied().enumerate() {
                let client = self.clone();
                tasks.spawn(async move { (slot, client.library_songs_page(at).await) });
            }

            // Collect by slot so the library keeps Apple's ordering regardless
            // of which request finishes first.
            let mut pages: Vec<Option<Vec<Track>>> = vec![None; LIBRARY_CONCURRENCY];
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

    async fn library_songs_page(&self, offset: usize) -> Result<Vec<Track>> {
        let res = self
            .get(&format!(
                "/me/library/songs?limit={LIBRARY_PAGE}&offset={offset}"
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
            .context("requesting library songs")?;

        if !res.status().is_success() {
            return Err(self.explain(res).await);
        }

        let parsed: Response<Resource<SongAttributes>> =
            res.json().await.context("decoding library songs")?;
        Ok(parsed.data.into_iter().map(Track::from).collect())
    }

    /// Catalog search. Needs only the developer token — no user token, no
    /// subscription — which makes it the cheapest way to prove the harvested
    /// token actually works before any playback is involved.
    /// `offset` walks further into the results. Apple caps `limit` at 25 per
    /// request for search, so anything past the first 25 needs paging.
    pub async fn search_songs(&self, term: &str, limit: u32, offset: usize) -> Result<Vec<Track>> {
        let query = urlencode(term);
        let res = self
            .get(&format!(
                "/catalog/{}/search?types=songs&limit={limit}&offset={offset}&term={query}",
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
        Ok(parsed
            .results
            .songs
            .map(|s| s.data.into_iter().map(Track::from).collect())
            .unwrap_or_default())
    }
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: SearchResults,
}

#[derive(Debug, Default, Deserialize)]
struct SearchResults {
    songs: Option<Response<Resource<SongAttributes>>>,
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
