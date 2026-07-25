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

pub struct Client {
    http: HttpClient,
    developer_token: String,
    music_user_token: Option<String>,
    storefront: String,
}

/// Failures the UI has a distinct response to. Anything else is a toast.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Apple Music rejected the session — sign in again")]
    Unauthorized,
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
            .bearer_auth(&self.developer_token);
        match &self.music_user_token {
            Some(t) => req.header("Music-User-Token", t.as_str()),
            None => req,
        }
    }

    /// Map a response status to something the UI can act on.
    fn diagnose(status: StatusCode) -> ApiError {
        match status {
            StatusCode::UNAUTHORIZED => ApiError::Unauthorized,
            StatusCode::FORBIDDEN => ApiError::Forbidden,
            StatusCode::NOT_FOUND => ApiError::NotFound,
            other => ApiError::Other(other),
        }
    }

    /// The user's saved songs. Paginated by Apple at 100; M5 walks the pages.
    pub async fn library_songs(&self, limit: u32) -> Result<Vec<Track>> {
        let res = self
            .get(&format!("/me/library/songs?limit={limit}"))
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
            return Err(Self::diagnose(res.status()).into());
        }

        let parsed: Response<Resource<SongAttributes>> =
            res.json().await.context("decoding library songs")?;
        Ok(parsed.data.into_iter().map(Track::from).collect())
    }

    /// Catalog search. Needs only the developer token — no user token, no
    /// subscription — which makes it the cheapest way to prove the harvested
    /// token actually works before any playback is involved.
    pub async fn search_songs(&self, term: &str, limit: u32) -> Result<Vec<Track>> {
        let query = urlencode(term);
        let res = self
            .get(&format!(
                "/catalog/{}/search?types=songs&limit={limit}&term={query}",
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
            return Err(Self::diagnose(res.status()).into());
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
            Client::diagnose(StatusCode::UNAUTHORIZED),
            ApiError::Unauthorized
        ));
        assert!(
            Client::diagnose(StatusCode::UNAUTHORIZED)
                .to_string()
                .contains("sign in again")
        );
        assert!(
            Client::diagnose(StatusCode::FORBIDDEN)
                .to_string()
                .contains("subscription")
        );
    }
}
