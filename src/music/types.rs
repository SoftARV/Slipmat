// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Our types. Apple's JSON stops here (CLAUDE.md rule 9) — "parse, don't
//! validate". `components/` sees only what's in this file.

use serde::Deserialize;

/// An Apple Music catalog id, e.g. `"1440857781"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub String);

impl std::fmt::Display for TrackId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct Track {
    /// The resource id. For library items this is a library id (`i.AbCd123`),
    /// which is **not** playable.
    pub id: TrackId,
    /// The id to hand MusicKit. Library resources carry their catalog
    /// equivalent in `playParams.catalogId`; catalog resources are already
    /// playable by their own id. `None` means a track that exists only in the
    /// user's library (an upload, or something delisted) and cannot be
    /// streamed — the UI must show it as unplayable rather than enqueue an id
    /// that silently does nothing.
    pub catalog_id: Option<String>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
    pub track_number: u32,
    pub artwork: Option<Artwork>,
}

impl Track {
    pub fn playable(&self) -> bool {
        self.catalog_id.is_some()
    }
}

impl Track {
    /// `3:42`, or `1:02:15` for anything over an hour.
    pub fn duration_label(&self) -> String {
        format_duration(self.duration_ms)
    }
}

pub fn format_duration(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Apple serves artwork as a **template** URL, not a fixed image:
/// `https://is1.mzstatic.com/image/thumb/…/{w}x{h}bb.jpg`. You substitute the
/// size you want, which is why we can ask for exactly the pixels the widget
/// needs instead of downscaling a 3000px jpeg.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artwork {
    template: String,
}

impl Artwork {
    pub fn new(template: impl Into<String>) -> Self {
        Self {
            template: template.into(),
        }
    }

    /// A concrete URL at `size`×`size`.
    ///
    /// Handles `{w}`/`{h}` and the `{f}` format placeholder some responses
    /// carry. Templates that contain no placeholder at all are returned as-is
    /// rather than mangled — Apple has served plain URLs before.
    pub fn url(&self, size: u32) -> String {
        self.template
            .replace("{w}", &size.to_string())
            .replace("{h}", &size.to_string())
            .replace("{f}", "jpg")
            .replace("{c}", "bb")
    }

    /// Cache key: stable per template, independent of requested size.
    pub fn cache_key(&self) -> String {
        // FNV-1a. Small, dependency-free, and we only need a filename — this is
        // not a security boundary.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in self.template.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{hash:016x}")
    }
}

/// A playlist or album as shown in the library list.
#[derive(Debug, Clone)]
pub struct Collection {
    pub id: String,
    pub name: String,
    pub subtitle: String,
    pub kind: CollectionKind,
    pub artwork: Option<Artwork>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionKind {
    Album,
    Playlist,
}

// --- Apple's wire shapes. Private: they never escape this module. ------------

#[derive(Debug, Deserialize)]
pub(crate) struct Response<T> {
    #[serde(default = "Vec::new")]
    pub data: Vec<T>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Resource<A> {
    pub id: String,
    pub attributes: Option<A>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SongAttributes {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub artist_name: String,
    #[serde(default)]
    pub album_name: String,
    #[serde(default)]
    pub duration_in_millis: u64,
    #[serde(default)]
    pub track_number: u32,
    pub artwork: Option<ArtworkAttributes>,
    pub play_params: Option<PlayParams>,
}

/// How Apple says "here is what to actually play".
///
/// For a catalog resource `id` is the catalog id. For a library resource `id`
/// is the library id and `catalog_id` holds the streamable equivalent — the
/// distinction that makes library playback work at all.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlayParams {
    #[serde(default)]
    pub id: String,
    pub catalog_id: Option<String>,
    #[serde(default)]
    pub is_library: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ArtworkAttributes {
    #[serde(default)]
    pub url: String,
}

impl From<Resource<SongAttributes>> for Track {
    fn from(res: Resource<SongAttributes>) -> Self {
        let attrs = res.attributes.unwrap_or(SongAttributes {
            name: String::new(),
            artist_name: String::new(),
            album_name: String::new(),
            duration_in_millis: 0,
            track_number: 0,
            artwork: None,
            play_params: None,
        });
        // A library resource's own id is not playable; its playParams carry the
        // catalog equivalent. A catalog resource is playable by its own id.
        let catalog_id = attrs.play_params.as_ref().and_then(|p| {
            p.catalog_id
                .clone()
                .or_else(|| (!p.is_library && !p.id.is_empty()).then(|| p.id.clone()))
        });
        Track {
            catalog_id,
            id: TrackId(res.id),
            title: attrs.name,
            artist: attrs.artist_name,
            album: attrs.album_name,
            duration_ms: attrs.duration_in_millis,
            track_number: attrs.track_number,
            artwork: attrs
                .artwork
                .filter(|a| !a.url.is_empty())
                .map(|a| Artwork::new(a.url)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artwork_template_substitution() {
        let art = Artwork::new("https://is1.mzstatic.com/image/thumb/abc/{w}x{h}bb.{f}");
        assert_eq!(
            art.url(256),
            "https://is1.mzstatic.com/image/thumb/abc/256x256bb.jpg"
        );
    }

    #[test]
    fn a_plain_url_survives_untouched() {
        let art = Artwork::new("https://example.com/cover.jpg");
        assert_eq!(art.url(512), "https://example.com/cover.jpg");
    }

    #[test]
    fn cache_key_is_stable_across_sizes_and_distinct_per_template() {
        let a = Artwork::new("https://is1.mzstatic.com/a/{w}x{h}bb.jpg");
        let b = Artwork::new("https://is1.mzstatic.com/b/{w}x{h}bb.jpg");
        assert_eq!(
            a.cache_key(),
            Artwork::new(a.url(0).replace("0x0", "{w}x{h}")).cache_key()
        );
        assert_ne!(a.cache_key(), b.cache_key());
        assert_eq!(a.cache_key().len(), 16, "usable as a filename");
    }

    #[test]
    fn duration_labels() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(42_000), "0:42");
        assert_eq!(format_duration(222_000), "3:42");
        assert_eq!(format_duration(3_735_000), "1:02:15");
    }

    #[test]
    fn a_library_track_is_played_by_its_catalog_id_not_its_own() {
        // Library ids look like `i.AbCd123` and are NOT playable. Enqueuing one
        // silently does nothing, which is the worst possible failure.
        let raw = r#"{"data":[{"id":"i.AbCd123","attributes":{"name":"SUPERESTRELLA",
            "artistName":"Aitana","playParams":{"id":"i.AbCd123","kind":"song",
            "isLibrary":true,"catalogId":"1799999999"}}}]}"#;
        let parsed: Response<Resource<SongAttributes>> = serde_json::from_str(raw).unwrap();
        let track = Track::from(parsed.data.into_iter().next().unwrap());

        assert_eq!(track.id, TrackId("i.AbCd123".into()));
        assert_eq!(track.catalog_id.as_deref(), Some("1799999999"));
        assert!(track.playable());
    }

    #[test]
    fn a_catalog_track_is_playable_by_its_own_id() {
        let raw = r#"{"data":[{"id":"1049009209","attributes":{"name":"Roundabout",
            "playParams":{"id":"1049009209","kind":"song"}}}]}"#;
        let parsed: Response<Resource<SongAttributes>> = serde_json::from_str(raw).unwrap();
        let track = Track::from(parsed.data.into_iter().next().unwrap());
        assert_eq!(track.catalog_id.as_deref(), Some("1049009209"));
    }

    #[test]
    fn a_library_only_upload_is_not_playable() {
        // No catalogId: an upload or a delisted track. It must report itself as
        // unplayable rather than hand MusicKit a library id that does nothing.
        let raw = r#"{"data":[{"id":"i.Local1","attributes":{"name":"Demo",
            "playParams":{"id":"i.Local1","kind":"song","isLibrary":true}}}]}"#;
        let parsed: Response<Resource<SongAttributes>> = serde_json::from_str(raw).unwrap();
        let track = Track::from(parsed.data.into_iter().next().unwrap());
        assert!(
            !track.playable(),
            "no catalog id means it cannot be streamed"
        );
    }

    #[test]
    fn parses_a_song_resource_with_missing_attributes() {
        // Library responses routinely omit fields; none of them may panic.
        let raw = r#"{"data":[{"id":"1440857781","attributes":{"name":"Roundabout",
            "artistName":"Yes","durationInMillis":513000,
            "artwork":{"url":"https://x/{w}x{h}bb.jpg"}}},{"id":"999"}]}"#;
        let parsed: Response<Resource<SongAttributes>> = serde_json::from_str(raw).unwrap();
        let tracks: Vec<Track> = parsed.data.into_iter().map(Track::from).collect();

        assert_eq!(tracks[0].title, "Roundabout");
        assert_eq!(tracks[0].album, "", "absent album is empty, not an error");
        assert_eq!(tracks[0].duration_label(), "8:33");
        assert!(tracks[0].artwork.is_some());

        assert_eq!(tracks[1].id, TrackId("999".into()));
        assert!(
            tracks[1].artwork.is_none(),
            "no attributes at all must be survivable"
        );
    }
}
