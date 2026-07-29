// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Our types. Apple's JSON stops here (CLAUDE.md rule 9) — "parse, don't
//! validate". `components/` sees only what's in this file.

use serde::{Deserialize, Serialize};

/// An Apple Music catalog id, e.g. `"1440857781"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TrackId(pub String);

impl std::fmt::Display for TrackId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// `PartialEq` is what lets a background refresh notice it changed nothing, and
// skip a rebuild that costs ~500ms of cover decoding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    /// When this was added to the library, ISO 8601, or empty. Sorts
    /// lexicographically, which for ISO 8601 is also chronologically — the one
    /// good reason to keep a date as a string.
    pub date_added: String,
    /// Release year, or empty when Apple did not say.
    pub year: String,
    /// Starred in Apple Music. Only ever true for a track read from the
    /// library — the catalog has no opinion about your favourites.
    pub favorite: bool,
    /// Already saved to the library, so "Add to Library" is not offered for it.
    /// Set by the client method that fetched it, never guessed.
    pub in_library: bool,
    /// The `i.…` id this track has **inside the library**, when it has one.
    ///
    /// Separate from `id` because a catalog row can be in the library too, and
    /// there `id` is the catalog id — handing that to the removal endpoint
    /// would be a well-formed request that deletes nothing. Populated from the
    /// `library` relationship when a request asked for it, or from the
    /// resource's own id when it was read out of `/me/library`.
    pub library_id: Option<String>,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
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

/// An album, as a search result or a page header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Album {
    pub id: String,
    /// When this was added to the library, ISO 8601, or empty. Sorts
    /// lexicographically, which for ISO 8601 is also chronologically.
    pub date_added: String,
    pub name: String,
    pub artist: String,
    pub artwork: Option<Artwork>,
    /// `2024`, or empty when Apple did not say.
    pub year: String,
    pub track_count: u32,
    /// True when `id` is a **library** id (`l.…`) rather than a catalog one.
    /// They are not interchangeable: a library id 404s against
    /// `/catalog/…/albums` and vice versa. Set explicitly by whichever client
    /// method parsed it, never sniffed from the id's shape.
    pub library: bool,
}

/// A playlist, as a grid tile or a page header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    /// ISO 8601, or empty. See `AlbumAttributes::date_added`.
    pub date_added: String,
    pub last_modified: String,
    pub name: String,
    /// Who made it — Apple's editors for a catalog playlist, **always empty for
    /// a library one**, which is the common case here. `curatorName` is a
    /// catalog attribute; `LibraryPlaylists.Attributes` does not carry it, so
    /// this is not something a request could ask harder for.
    pub curator: String,
    /// Apple's blurb, plain text. Empty far more often than not.
    ///
    /// **Parsed, and deliberately not shown anywhere.** It reads like the
    /// obvious subtitle for a playlist with no curator, and it is not: these
    /// are paragraphs, and one under the title pushed the cover, the buttons
    /// and the whole track list off the bottom of the window. Both the tile and
    /// the page had to learn this separately. If it ever earns a place it is
    /// somewhere with room for a sentence, not a header line.
    pub description: String,
    pub artwork: Option<Artwork>,
    /// As [`Album::library`].
    pub library: bool,
}

/// An artist, as a search result or a page header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub artwork: Option<Artwork>,
    /// Apple's genre list, joined — "Pop, Latin".
    pub genres: String,
    /// As [`Album::library`]. A library artist has no artwork of its own —
    /// Apple returns only a name for `/me/library/artists` — so the client asks
    /// for the catalog twin inline and copies its portrait across. See
    /// [`LibraryArtistResource`].
    pub library: bool,
}

// --- Apple's wire shapes. Private: they never escape this module. ------------

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Response<T> {
    #[serde(default = "Vec::new")]
    pub data: Vec<T>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Resource<A> {
    pub id: String,
    pub attributes: Option<A>,
    /// Only populated when the request asked for `include=library`, and only
    /// on endpoints that honour it — catalog search does **not**, though album
    /// and playlist track relationships do.
    #[serde(default)]
    pub relationships: Option<ResourceRelationships>,
}

/// The one relationship we read off a catalog resource: whether it is in the
/// user's library. A non-empty `data` means yes.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ResourceRelationships {
    pub library: Option<RelationshipData>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RelationshipData {
    #[serde(default)]
    pub data: Vec<RelationshipRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RelationshipRef {
    pub id: String,
}

impl<A> Resource<A> {
    /// Whether Apple said this is in the library.
    ///
    /// `None` when the question was not asked — which is different from "no",
    /// and is why this is an `Option` rather than a bool defaulting to false.
    pub fn library_membership(&self) -> Option<bool> {
        let library = self.relationships.as_ref()?.library.as_ref()?;
        Some(!library.data.is_empty())
    }

    /// The library id this catalog resource maps to, if it is in the library.
    pub fn library_id(&self) -> Option<String> {
        let library = self.relationships.as_ref()?.library.as_ref()?;
        library.data.first().map(|r| r.id.clone())
    }
}

/// An album with its `include=tracks` relationship attached.
#[derive(Debug, Deserialize)]
pub(crate) struct AlbumResource {
    pub id: String,
    pub attributes: Option<AlbumAttributes>,
    pub relationships: Option<AlbumRelationships>,
}

impl AlbumResource {
    pub fn into_album(self) -> Resource<AlbumAttributes> {
        Resource {
            id: self.id,
            attributes: self.attributes,
            relationships: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct AlbumRelationships {
    pub tracks: Option<Response<Resource<SongAttributes>>>,
}

/// An artist with its `include=albums` relationship attached.
#[derive(Debug, Deserialize)]
pub(crate) struct ArtistResource {
    pub id: String,
    pub attributes: Option<ArtistAttributes>,
    pub relationships: Option<ArtistRelationships>,
}

impl ArtistResource {
    pub fn into_artist(self) -> Resource<ArtistAttributes> {
        Resource {
            id: self.id,
            attributes: self.attributes,
            relationships: None,
        }
    }
}

/// A **library** artist, with its catalog counterpart pulled in.
///
/// Apple returns only a name for `/me/library/artists` — no artwork, no genres.
/// The picture the web player shows comes from the *catalog* artist, which is
/// reachable as a relationship on the library one, so asking for it inline
/// costs no extra requests.
#[derive(Debug, Deserialize)]
pub(crate) struct LibraryArtistResource {
    pub id: String,
    pub attributes: Option<ArtistAttributes>,
    pub relationships: Option<LibraryArtistRelationships>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LibraryArtistRelationships {
    pub catalog: Option<Response<Resource<ArtistAttributes>>>,
}

impl From<LibraryArtistResource> for Artist {
    fn from(res: LibraryArtistResource) -> Self {
        // The catalog twin, when Apple honoured `include=catalog`.
        let catalog = res
            .relationships
            .and_then(|r| r.catalog)
            .and_then(|c| c.data.into_iter().next())
            .map(Artist::from);

        let mut artist = Artist::from(Resource {
            // The **library** id: it is what the library artist page is opened
            // with. The catalog twin's id would 404 there.
            id: res.id,
            attributes: res.attributes,
            relationships: None,
        });
        artist.library = true;

        if let Some(catalog) = catalog {
            // Name stays the library's — it is what the user's own library
            // calls this artist. Everything the library does not carry comes
            // from the catalog.
            artist.artwork = catalog.artwork;
            artist.genres = catalog.genres;
            if artist.name.is_empty() {
                artist.name = catalog.name;
            }
        }
        artist
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ArtistRelationships {
    pub albums: Option<Response<Resource<AlbumAttributes>>>,
    /// Only ever present on a **library** artist asked with `include=catalog`
    /// — that is where the portrait lives. Absent on a catalog artist, which
    /// already has its own artwork.
    pub catalog: Option<Response<Resource<ArtistAttributes>>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SongAttributes {
    #[serde(default)]
    pub name: String,
    /// When the user saved it, if Apple ever tells us.
    ///
    /// **It does not.** `dateAdded` is not in `LibrarySongs.Attributes` and
    /// `extend=dateAdded` does not produce it — measured as 0 of 541 tracks
    /// against a real library. Kept as the place it would land, and as the
    /// reason there is no "Recently Added" sort.
    #[serde(default)]
    pub date_added: String,
    /// `"2016-05-27"` or `"2016"`; we only ever want the year.
    #[serde(default)]
    pub release_date: String,
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
    /// Whether the user has starred it. A documented attribute of
    /// `LibrarySongs.Attributes`, so it costs nothing — no read-back, no extra
    /// request. Absent on catalog songs.
    #[serde(default)]
    pub in_favorites: bool,
}

/// How Apple says "here is what to actually play".
///
/// For a catalog resource `id` is the catalog id. For a library resource `id`
/// is the library id and `catalog_id` holds the streamable equivalent — the
/// distinction that makes library playback work at all.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlayParams {
    #[serde(default)]
    pub id: String,
    pub catalog_id: Option<String>,
    #[serde(default)]
    pub is_library: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AlbumAttributes {
    /// Documented on `LibraryAlbums.Attributes`, and **that is not evidence
    /// it arrives** — `dateAdded` is documented for library songs too and was
    /// measured as 0 of 541. Parsed so the question can be counted rather
    /// than argued about.
    #[serde(default)]
    pub date_added: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub artist_name: String,
    pub artwork: Option<ArtworkAttributes>,
    /// `"2024-03-15"` or `"2024"` — we only ever want the year.
    #[serde(default)]
    pub release_date: String,
    #[serde(default)]
    pub track_count: u32,
}

impl From<Resource<AlbumAttributes>> for Album {
    fn from(res: Resource<AlbumAttributes>) -> Self {
        let a = res.attributes;
        Album {
            id: res.id,
            date_added: a.as_ref().map(|a| a.date_added.clone()).unwrap_or_default(),
            name: a.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
            artist: a
                .as_ref()
                .map(|a| a.artist_name.clone())
                .unwrap_or_default(),
            // Apple gives a full date, a bare year, or nothing at all.
            year: a
                .as_ref()
                .map(|a| a.release_date.chars().take(4).collect())
                .unwrap_or_default(),
            track_count: a.as_ref().map(|a| a.track_count).unwrap_or(0),
            // Catalog unless a library method says otherwise.
            library: false,
            artwork: a
                .and_then(|a| a.artwork)
                .filter(|art| !art.url.is_empty())
                .map(|art| Artwork::new(art.url)),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArtistAttributes {
    #[serde(default)]
    pub name: String,
    pub artwork: Option<ArtworkAttributes>,
    #[serde(default)]
    pub genre_names: Vec<String>,
}

impl From<Resource<ArtistAttributes>> for Artist {
    fn from(res: Resource<ArtistAttributes>) -> Self {
        let a = res.attributes;
        Artist {
            id: res.id,
            name: a.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
            genres: a
                .as_ref()
                .map(|a| a.genre_names.join(", "))
                .unwrap_or_default(),
            library: false,
            artwork: a
                .and_then(|a| a.artwork)
                .filter(|art| !art.url.is_empty())
                .map(|art| Artwork::new(art.url)),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlaylistAttributes {
    /// Same question as `AlbumAttributes::date_added`, same reason for
    /// counting rather than assuming.
    #[serde(default)]
    pub date_added: String,
    /// Documented on `LibraryPlaylists.Attributes` only. A playlist you edit
    /// is one whose order changed, which is a different and arguably more
    /// useful "recent" than when you first saved it.
    #[serde(default)]
    pub last_modified_date: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub curator_name: String,
    pub artwork: Option<ArtworkAttributes>,
    pub description: Option<DescriptionAttribute>,
}

/// Apple wraps a playlist's blurb in an object with `standard` and sometimes
/// `short`. Both are absent on a playlist you made yourself.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DescriptionAttribute {
    #[serde(default)]
    pub standard: String,
}

impl From<Resource<PlaylistAttributes>> for Playlist {
    fn from(res: Resource<PlaylistAttributes>) -> Self {
        let a = res.attributes;
        Playlist {
            id: res.id,
            date_added: a.as_ref().map(|a| a.date_added.clone()).unwrap_or_default(),
            last_modified: a
                .as_ref()
                .map(|a| a.last_modified_date.clone())
                .unwrap_or_default(),
            name: a.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
            curator: a
                .as_ref()
                .map(|a| a.curator_name.clone())
                .unwrap_or_default(),
            description: a
                .as_ref()
                .and_then(|a| a.description.as_ref())
                .map(|d| d.standard.clone())
                .unwrap_or_default(),
            artwork: a
                .and_then(|a| a.artwork)
                .filter(|art| !art.url.is_empty())
                .map(|art| Artwork::new(art.url)),
            // Set by whichever client method fetched it, as for Album/Artist.
            library: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ArtworkAttributes {
    #[serde(default)]
    pub url: String,
}

impl From<Resource<SongAttributes>> for Track {
    fn from(res: Resource<SongAttributes>) -> Self {
        // Read before `attributes` is moved out below. When the request asked
        // for `include=library`, Apple answers it here — so a catalog row can
        // know it is already saved without a second call. `None` means the
        // question was not asked, which is not the same as "no".
        let membership = res.library_membership();
        let library_id = res.library_id();
        let attrs = res.attributes.unwrap_or(SongAttributes {
            name: String::new(),
            date_added: String::new(),
            release_date: String::new(),
            in_favorites: false,
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
            date_added: attrs.date_added,
            favorite: attrs.in_favorites,
            in_library: membership.unwrap_or(false),
            library_id,
            year: attrs.release_date.chars().take(4).collect(),
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
    fn artwork_survives_a_round_trip_with_its_cache_key_intact() {
        // The cache key is derived from the template, and it names the file on
        // disk. If a round trip through the library cache perturbed the
        // template, every cover in `~/.cache/slipmat/artwork` would miss and
        // opening from cache would re-download the lot — the opposite of the
        // point.
        let art = Artwork::new("https://is1.mzstatic.com/image/thumb/x/{w}x{h}{c}.{f}");
        let json = serde_json::to_string(&art).unwrap();
        // `transparent`, so it is the URL itself and not `{"template":…}`.
        assert!(json.starts_with('"'), "{json}");
        let back: Artwork = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cache_key(), art.cache_key());
        assert_eq!(back.url(96), art.url(96));
    }

    #[test]
    fn a_star_makes_two_tracks_unequal() {
        // `Track: PartialEq` is what decides whether a background refresh
        // rebuilds. If it ignored a field Apple can change, a library that
        // gained a favourite would keep showing the old row until reload.
        let track = |favorite| Track {
            date_added: "2024-01-01T00:00:00Z".into(),
            year: "2024".into(),
            favorite,
            in_library: true,
            library_id: None,
            id: TrackId("i.a".into()),
            catalog_id: Some("1".into()),
            title: "Bloom".into(),
            artist: "Radiohead".into(),
            album: "The King of Limbs".into(),
            duration_ms: 1,
            track_number: 1,
            artwork: None,
        };
        assert_eq!(track(true), track(true));
        assert_ne!(track(true), track(false));
    }

    #[test]
    fn an_album_keeps_only_the_year_from_a_release_date() {
        // Apple sends a full date, a bare year, or nothing.
        let raw = r#"{"data":[{"id":"1","attributes":{"name":"Fragile","artistName":"Yes",
            "releaseDate":"1971-11-26","trackCount":9}}]}"#;
        let parsed: Response<Resource<AlbumAttributes>> = serde_json::from_str(raw).unwrap();
        let album = Album::from(parsed.data.into_iter().next().unwrap());
        assert_eq!(album.year, "1971");
        assert_eq!(album.track_count, 9);
        assert_eq!(album.artist, "Yes");
    }

    #[test]
    fn an_album_with_no_attributes_at_all_is_survivable() {
        let parsed: Response<Resource<AlbumAttributes>> =
            serde_json::from_str(r#"{"data":[{"id":"1"}]}"#).unwrap();
        let album = Album::from(parsed.data.into_iter().next().unwrap());
        assert_eq!(album.id, "1");
        assert_eq!(album.year, "");
        assert!(album.artwork.is_none());
    }

    #[test]
    fn artist_genres_are_joined_for_display() {
        let raw = r#"{"data":[{"id":"9","attributes":{"name":"Aitana",
            "genreNames":["Pop","Latin"]}}]}"#;
        let parsed: Response<Resource<ArtistAttributes>> = serde_json::from_str(raw).unwrap();
        let artist = Artist::from(parsed.data.into_iter().next().unwrap());
        assert_eq!(artist.genres, "Pop, Latin");
    }

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

    #[test]
    fn a_library_artist_keeps_its_own_id_and_takes_the_catalog_portrait() {
        // The library id is what opens the library artist page; the catalog
        // twin's id would 404 there. Only the things the library does not
        // carry — artwork, genres — come across.
        let resource = LibraryArtistResource {
            id: "r.abc".into(),
            attributes: Some(ArtistAttributes {
                name: "Aitana".into(),
                artwork: None,
                genre_names: Vec::new(),
            }),
            relationships: Some(LibraryArtistRelationships {
                catalog: Some(Response {
                    data: vec![Resource {
                        id: "1234".into(),
                        relationships: None,
                        attributes: Some(ArtistAttributes {
                            name: "Aitana".into(),
                            artwork: Some(ArtworkAttributes {
                                url: "https://example.test/{w}x{h}bb.jpg".into(),
                            }),
                            genre_names: vec!["Pop".into(), "Latin".into()],
                        }),
                    }],
                }),
            }),
        };

        let artist = Artist::from(resource);
        assert_eq!(artist.id, "r.abc", "the library id opens the library page");
        assert!(artist.library);
        assert!(artist.artwork.is_some(), "portrait comes from the catalog");
        assert_eq!(artist.genres, "Pop, Latin");
    }

    #[test]
    fn a_library_artist_without_a_catalog_twin_still_parses() {
        // `include=catalog` is honoured today and might not be tomorrow. No
        // portrait is a placeholder, not a failure.
        let artist = Artist::from(LibraryArtistResource {
            id: "r.abc".into(),
            attributes: Some(ArtistAttributes {
                name: "Aitana".into(),
                artwork: None,
                genre_names: Vec::new(),
            }),
            relationships: None,
        });
        assert_eq!(artist.name, "Aitana");
        assert!(artist.artwork.is_none());
        assert!(artist.library);
    }

    #[test]
    fn a_playlist_you_made_yourself_has_no_curator_or_blurb() {
        // The common case in a library: Apple returns a name and nothing else.
        // Both come back empty rather than absent, so the tile and the page
        // header can just ask whether they are empty.
        let playlist = Playlist::from(Resource {
            id: "p.abc".into(),
            relationships: None,
            attributes: Some(PlaylistAttributes {
                date_added: String::new(),
                last_modified_date: String::new(),
                name: "Late night".into(),
                curator_name: String::new(),
                artwork: None,
                description: None,
            }),
        });
        assert_eq!(playlist.name, "Late night");
        assert!(playlist.curator.is_empty());
        assert!(playlist.description.is_empty());
        assert!(playlist.artwork.is_none());
        // Set by the client method that fetched it, never here.
        assert!(!playlist.library);
    }

    #[test]
    fn membership_is_read_from_the_relationship_not_guessed() {
        // `include=library` on a catalog song is how a search result learns it
        // is already saved. The library id must come from the relationship,
        // never from the resource's own id — that one is a catalog id, and the
        // removal endpoint would accept it and delete nothing.
        let json = r#"{
            "id": "282559791",
            "attributes": {"name": "Avril 14th", "artistName": "Aphex Twin"},
            "relationships": {"library": {"data": [{"id": "i.RBrxxaLS1BA3Jv5"}]}}
        }"#;
        let track = Track::from(serde_json::from_str::<Resource<SongAttributes>>(json).unwrap());
        assert!(track.in_library);
        assert_eq!(track.library_id.as_deref(), Some("i.RBrxxaLS1BA3Jv5"));
        assert_eq!(track.id.0, "282559791", "the catalog id stays the id");
    }

    #[test]
    fn an_empty_relationship_means_not_in_the_library() {
        let json = r#"{"id": "1", "attributes": {"name": "x"},
                       "relationships": {"library": {"data": []}}}"#;
        let track = Track::from(serde_json::from_str::<Resource<SongAttributes>>(json).unwrap());
        assert!(!track.in_library);
        assert_eq!(track.library_id, None);
    }

    #[test]
    fn no_relationship_at_all_is_not_a_no() {
        // Catalog search omits it entirely — measured — so its absence must
        // read as "not asked", not as a positive "no".
        let track = Track::from(
            serde_json::from_str::<Resource<SongAttributes>>(
                r#"{"id": "1", "attributes": {"name": "x"}}"#,
            )
            .unwrap(),
        );
        assert!(!track.in_library);
        assert_eq!(track.library_id, None);
    }

    #[test]
    fn an_editorial_playlist_keeps_its_curator_and_blurb() {
        let playlist = Playlist::from(Resource {
            id: "pl.123".into(),
            relationships: None,
            attributes: Some(PlaylistAttributes {
                date_added: String::new(),
                last_modified_date: String::new(),
                name: "Today's Hits".into(),
                curator_name: "Apple Music".into(),
                artwork: Some(ArtworkAttributes {
                    url: "https://example.test/{w}x{h}bb.jpg".into(),
                }),
                description: Some(DescriptionAttribute {
                    standard: "The songs everyone is playing.".into(),
                }),
            }),
        });
        assert_eq!(playlist.curator, "Apple Music");
        assert_eq!(playlist.description, "The songs everyone is playing.");
        assert!(playlist.artwork.is_some());
    }
}
