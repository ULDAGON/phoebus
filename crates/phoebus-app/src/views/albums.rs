//! Albums: every album in the library, sorted by artist then title — with the hearted ones
//! pinned to the front (UI-SPEC v1.3 §Favorites).
//!
//! **Only this view pins.** Recently Added is ordered by `added_at` and that order *is* the
//! point of the page, so a favourite must not jump the queue there; the Artists page and
//! the Search grid show a subset of one artist / one query, where a pin would only scramble
//! a list the user is reading top to bottom.

use egui::Ui;
use phoebus_core::{AlbumKey, Favorites, Library};

use crate::nav::Ctx;
use crate::views;
use crate::widgets::album_card;

/// The pinned order, cached.
///
/// `pinned_albums` allocates a `Vec<AlbumKey>` of the whole library and clones a
/// two-`String` key into every slot — 290 heap allocations per frame on a real library, for
/// an answer that only changes when a heart is clicked or a scan lands. So it is computed
/// on change and cached here, exactly like `songs::State`'s sort order, and for exactly the
/// same reason.
#[derive(Default)]
pub struct State {
    keys: Vec<AlbumKey>,
    dirty: bool,
}

impl State {
    /// Drop the cached order. Called by [`Action::ToggleFavAlbum`](crate::nav::Action) and
    /// by every rescan — one changes which albums are pinned, the other changes which
    /// albums there are.
    pub fn invalidate(&mut self) {
        self.keys = Vec::new();
        self.dirty = true;
    }

    /// The order to draw, pinning at most once per change.
    ///
    /// With nothing hearted the pinned order *is* `Library::albums()`, so the common case
    /// borrows the library's own slice and owns nothing at all — the same shape
    /// `songs::State::rows` gives its default sort.
    fn keys<'a>(&'a mut self, lib: &'a Library, favs: &Favorites) -> &'a [AlbumKey] {
        if favs.album_count() == 0 {
            return lib.albums();
        }
        if self.dirty {
            self.keys = phoebus_core::pinned_albums(lib, favs);
            self.dirty = false;
        }
        &self.keys
    }
}

/// Draw the view.
pub fn show(ui: &mut Ui, cx: &mut Ctx, st: &mut State) {
    views::page(ui, |ui| {
        views::heading(ui, "ALBUMS");
        if cx.lib.album_count() == 0 {
            views::centered_note(ui, &["NO ALBUMS YET"]);
            return;
        }
        // Copy the shared references out of `cx` so the slice does not borrow `cx` itself.
        let (lib, favs) = (cx.lib, cx.favs);
        album_card::grid(ui, cx, st.keys(lib, favs), "albums-grid");
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoebus_core::Track;

    fn lib() -> Library {
        let mut tracks = Vec::new();
        for rel in [
            "Ann/Aleph/01 a.m4a",
            "Bob/Beth/01 b.m4a",
            "Cal/Gimel/01 c.m4a",
        ] {
            let mut t = Track::new(rel);
            t.refresh_key();
            tracks.push(t);
        }
        Library::build("/lib", tracks)
    }

    fn favorites() -> Favorites {
        let mut favs = Favorites::load_from(std::path::Path::new(
            "/nonexistent/phoebus-albums-test/favorites.json",
        ));
        favs.set_ephemeral(true);
        favs
    }

    /// Nothing hearted: the library's own slice, no `Vec`, no clones.
    #[test]
    fn the_unpinned_order_allocates_nothing() {
        let l = lib();
        let favs = favorites();
        let mut st = State::default();
        assert_eq!(st.keys(&l, &favs), l.albums());
        assert!(st.keys.is_empty(), "the common case must not allocate");
    }

    /// A hearted album goes first and the rest keep their sorted order — and the order is
    /// built once, not once per frame.
    #[test]
    fn hearting_pins_and_invalidates_exactly_once() {
        let l = lib();
        let mut favs = favorites();
        let second = l.albums()[1].clone();
        favs.toggle_album(&second);

        let mut st = State::default();
        st.invalidate();
        assert!(st.dirty);
        let order: Vec<AlbumKey> = st.keys(&l, &favs).to_vec();
        assert_eq!(order[0], second, "the hearted album is pinned to the front");
        assert_eq!(
            order[1..].to_vec(),
            vec![l.albums()[0].clone(), l.albums()[2].clone()],
            "everything else keeps its relative sorted order"
        );
        assert!(!st.dirty, "the pinned order is cached");

        // A second call must not rebuild it…
        st.keys[0] = l.albums()[2].clone();
        assert_eq!(
            st.keys(&l, &favs)[0],
            l.albums()[2],
            "the cache was rebuilt when nothing had changed"
        );
        // …until something invalidates it, which is what a toggle does.
        st.invalidate();
        assert_eq!(st.keys(&l, &favs)[0], second);

        // Unhearting the last favourite drops straight back to the borrowed slice.
        favs.toggle_album(&second);
        assert_eq!(st.keys(&l, &favs), l.albums());
    }
}
