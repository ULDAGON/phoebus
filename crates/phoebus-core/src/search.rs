//! Library search: case-insensitive substring matching with prefix matches ranked first,
//! each section capped.

use crate::model::{AlbumKey, Library, TrackId};

/// Maximum number of hits returned per section.
pub const SECTION_CAP: usize = 50;

/// The three sections of the Search view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchResults {
    /// Matching artist display names, best first.
    pub artists: Vec<String>,
    /// Matching album keys, best first.
    pub albums: Vec<AlbumKey>,
    /// Matching track ids, best first.
    pub tracks: Vec<TrackId>,
}

impl SearchResults {
    /// True when no section has a hit (the view then shows `NO RESULTS`).
    pub fn is_empty(&self) -> bool {
        self.artists.is_empty() && self.albums.is_empty() && self.tracks.is_empty()
    }

    /// Total number of hits across all sections.
    pub fn len(&self) -> usize {
        self.artists.len() + self.albums.len() + self.tracks.len()
    }
}

/// Search `library` for `query`, capping each section at [`SECTION_CAP`].
///
/// Artists match on name; albums on title (then album artist); tracks on title (then artist,
/// then album). Within a section, prefix matches come first, then substring matches on the
/// primary field, then matches on a secondary field — ties keep the library's own order.
pub fn search(library: &Library, query: &str) -> SearchResults {
    search_capped(library, query, SECTION_CAP)
}

/// [`search`] with an explicit per-section cap.
pub fn search_capped(library: &Library, query: &str, cap: usize) -> SearchResults {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return SearchResults::default();
    }

    let mut artists: Vec<(u8, &str)> = Vec::new();
    for a in library.artists() {
        if let Some(rank) = rank(&a.name, &needle) {
            artists.push((rank, a.name.as_str()));
        }
    }

    let mut albums: Vec<(u8, &AlbumKey)> = Vec::new();
    for key in library.albums() {
        let Some(album) = library.album(key) else {
            continue;
        };
        let r = rank(&album.title, &needle).or_else(|| rank(&album.artist, &needle).map(|_| 2));
        if let Some(r) = r {
            albums.push((r, key));
        }
    }

    let mut tracks: Vec<(u8, TrackId)> = Vec::new();
    for id in library.tracks_sorted() {
        let Some(t) = library.track(*id) else {
            continue;
        };
        let r = rank(&t.title, &needle)
            .or_else(|| rank(&t.artist, &needle).map(|_| 2))
            .or_else(|| rank(&t.album, &needle).map(|_| 3));
        if let Some(r) = r {
            tracks.push((r, *id));
        }
    }

    artists.sort_by_key(|(r, _)| *r);
    albums.sort_by_key(|(r, _)| *r);
    tracks.sort_by_key(|(r, _)| *r);

    SearchResults {
        artists: artists
            .into_iter()
            .take(cap)
            .map(|(_, n)| n.to_string())
            .collect(),
        albums: albums
            .into_iter()
            .take(cap)
            .map(|(_, k)| k.clone())
            .collect(),
        tracks: tracks.into_iter().take(cap).map(|(_, id)| id).collect(),
    }
}

/// Every track whose **title or artist** contains `query`, in the library's own order.
///
/// Deliberately not [`search`]. The add-songs picker (UI-SPEC v1.4 §Add songs) is a list of
/// the whole library that narrows as the user types, and [`search`] disagrees with that on
/// all three counts: it also matches the *album* (so typing `odyssey` would offer every
/// track of an album whose name the user never wrote), it re-orders by rank (so rows jump
/// between keystrokes instead of staying where they were), and it caps each section at
/// [`SECTION_CAP`] (so a 200-song artist would silently show 50). An empty query means
/// *everything* here, where in [`search`] it means *nothing*.
///
/// One linear pass over [`Library::tracks_sorted`] with an allocation-free case-insensitive
/// `contains` per field — cheap enough to run on each keystroke, which is what the picker
/// does (it caches the result and recomputes only when the query changes).
pub fn filter_tracks(library: &Library, query: &str) -> Vec<TrackId> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return library.tracks_sorted().to_vec();
    }
    library
        .tracks_sorted()
        .iter()
        .copied()
        .filter(|id| {
            library
                .track(*id)
                .is_some_and(|t| contains_ci(&t.title, &needle) || contains_ci(&t.artist, &needle))
        })
        .collect()
}

/// `0` = prefix match, `1` = substring match, `None` = no match. `needle` must be lowercase.
fn rank(haystack: &str, needle: &str) -> Option<u8> {
    if starts_with_ci(haystack, needle) {
        Some(0)
    } else if contains_ci(haystack, needle) {
        Some(1)
    } else {
        None
    }
}

/// Case-insensitive `starts_with`, allocation-free. `needle` must already be lowercase.
fn starts_with_ci(haystack: &str, needle: &str) -> bool {
    let mut hay = haystack.chars().flat_map(char::to_lowercase);
    for want in needle.chars() {
        match hay.next() {
            Some(got) if got == want => {}
            _ => return false,
        }
    }
    true
}

/// Case-insensitive `contains`, allocation-free. `needle` must already be lowercase.
fn contains_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .char_indices()
        .any(|(i, _)| starts_with_ci(&haystack[i..], needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Track;
    use std::time::Duration;

    fn track(rel: &str, title: &str, artist: &str, album: &str) -> Track {
        let mut t = Track::new(rel);
        t.title = title.to_string();
        t.artist = artist.to_string();
        t.album_artist = artist.to_string();
        t.album = album.to_string();
        t.duration = Duration::from_secs(60);
        t.refresh_key();
        t
    }

    fn lib() -> Library {
        Library::build(
            "/lib",
            vec![
                track("HOME/Odyssey/01 Intro.m4a", "Intro", "HOME", "Odyssey"),
                track(
                    "HOME/Odyssey/02 Resonance.m4a",
                    "Resonance",
                    "HOME",
                    "Odyssey",
                ),
                track("Woodkid/S16/01 Goliath.m4a", "Goliath", "Woodkid", "S16"),
                track(
                    "Letters/Winter/01 Let It Be.m4a",
                    "Let It Be",
                    "Letters",
                    "Winter",
                ),
                track(
                    "Letters/Winter/02 Violet.m4a",
                    "Violet",
                    "Letters",
                    "Winter",
                ),
            ],
        )
    }

    fn titles(lib: &Library, r: &SearchResults) -> Vec<String> {
        r.tracks
            .iter()
            .filter_map(|id| lib.track(*id))
            .map(|t| t.title.clone())
            .collect()
    }

    #[test]
    fn empty_query_returns_nothing() {
        let l = lib();
        assert!(search(&l, "").is_empty());
        assert!(search(&l, "   ").is_empty());
        assert_eq!(search(&l, "zzz").len(), 0);
    }

    #[test]
    fn matching_is_case_insensitive_substring() {
        let l = lib();
        let r = search(&l, "SONA"); // inside "Resonance"
        assert_eq!(titles(&l, &r), vec!["Resonance"]);
        let r = search(&l, "goliath");
        assert_eq!(titles(&l, &r), vec!["Goliath"]);
    }

    #[test]
    fn prefix_matches_rank_before_substring_matches() {
        let l = lib();
        let r = search(&l, "let");
        // "Let It Be" starts with the query, "Violet" only contains it.
        assert_eq!(titles(&l, &r), vec!["Let It Be", "Violet"]);
        assert_eq!(r.artists, vec!["Letters".to_string()]);
        // The album matches only through its artist, so it still shows up.
        assert_eq!(r.albums, vec![AlbumKey::new("Letters", "Winter")]);
    }

    #[test]
    fn albums_match_on_title_then_artist() {
        let l = lib();
        let r = search(&l, "odyssey");
        assert_eq!(r.albums, vec![AlbumKey::new("HOME", "Odyssey")]);
        let r = search(&l, "woodkid");
        assert_eq!(r.albums, vec![AlbumKey::new("Woodkid", "S16")]);
        assert_eq!(r.artists, vec!["Woodkid".to_string()]);
        assert_eq!(
            titles(&l, &r),
            vec!["Goliath"],
            "tracks match on artist too"
        );
    }

    #[test]
    fn tracks_match_on_album_name() {
        let l = lib();
        let r = search(&l, "s16");
        assert_eq!(titles(&l, &r), vec!["Goliath"]);
        assert!(r.artists.is_empty());
    }

    #[test]
    fn sections_are_capped() {
        let tracks: Vec<Track> = (0..120)
            .map(|i| {
                track(
                    &format!("A{i}/Alb{i}/01 Song{i}.m4a"),
                    &format!("Song{i}"),
                    &format!("Artist{i}"),
                    &format!("Alb{i}"),
                )
            })
            .collect();
        let l = Library::build("/lib", tracks);
        let r = search(&l, "song");
        assert_eq!(r.tracks.len(), SECTION_CAP);
        let r = search_capped(&l, "artist", 7);
        assert_eq!(r.artists.len(), 7);
        assert_eq!(r.tracks.len(), 7);
    }

    /// The add-songs picker's filter, in full (UI-SPEC v1.4 §Add songs). Every clause is one
    /// way it deliberately differs from [`search`].
    #[test]
    fn the_picker_filter_is_title_or_artist_substring_in_library_order() {
        let l = lib();
        let all = l.tracks_sorted().to_vec();

        // No query is the whole library, in the Songs view's order — not "no results".
        assert_eq!(filter_tracks(&l, ""), all);
        assert_eq!(filter_tracks(&l, "   "), all);

        // Case-insensitive substring on the title…
        assert_eq!(names(&l, &filter_tracks(&l, "SONA")), vec!["Resonance"]);
        // …and on the artist.
        assert_eq!(names(&l, &filter_tracks(&l, "woodkid")), vec!["Goliath"]);

        // But NOT on the album: `search` returns Goliath for `s16`, this must not.
        assert_eq!(titles(&l, &search(&l, "s16")), vec!["Goliath"]);
        assert!(filter_tracks(&l, "s16").is_empty());

        // No ranking: `let` hits "Let It Be" and "Violet", and they stay in library order
        // rather than being re-sorted prefix-first the way `search` sorts them.
        assert_eq!(
            names(&l, &filter_tracks(&l, "let")),
            vec!["Let It Be", "Violet"]
        );
        assert_eq!(titles(&l, &search(&l, "let")), vec!["Let It Be", "Violet"]);
        assert_eq!(
            filter_tracks(&l, "let"),
            all.iter()
                .copied()
                .filter(|id| filter_tracks(&l, "let").contains(id))
                .collect::<Vec<_>>(),
            "the hits are a subsequence of `tracks_sorted`"
        );

        assert!(filter_tracks(&l, "zzz").is_empty());
    }

    /// A hit count past [`SECTION_CAP`] is shown in full — the picker scrolls, it does not
    /// truncate.
    #[test]
    fn the_picker_filter_is_not_capped() {
        let tracks: Vec<Track> = (0..120)
            .map(|i| {
                track(
                    &format!("A{i}/Alb{i}/01 Song{i}.m4a"),
                    &format!("Song{i}"),
                    &format!("Artist{i}"),
                    &format!("Alb{i}"),
                )
            })
            .collect();
        let l = Library::build("/lib", tracks);
        assert_eq!(search(&l, "song").tracks.len(), SECTION_CAP);
        assert_eq!(filter_tracks(&l, "song").len(), 120);
        assert_eq!(filter_tracks(&l, "").len(), 120);
    }

    fn names(lib: &Library, ids: &[TrackId]) -> Vec<String> {
        ids.iter()
            .filter_map(|id| lib.track(*id))
            .map(|t| t.title.clone())
            .collect()
    }

    #[test]
    fn ci_helpers_handle_unicode_and_boundaries() {
        assert!(starts_with_ci("Über", "üb"));
        assert!(contains_ci("Café Noir", "é n"));
        assert!(!contains_ci("abc", "abcd"));
        assert!(contains_ci("abc", ""));
    }
}
