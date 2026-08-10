//! Gate G1 probe: scan the real library and print what the app will see.
//! Read-only apart from the cover cache in the app-data directory.

use phoebus_core::{AppState, Dirs, paths, scanner, search};

fn main() {
    let dirs = Dirs::resolve();
    let state = AppState::load_from(&dirs.state_path());
    let root = paths::library_root_for(state.configured_library_root());
    println!("root: {}", root.display());
    println!("data: {}", dirs.data_dir().display());
    let lib = scanner::scan_with_covers(&root, &dirs.covers_dir());

    println!(
        "tracks={} albums={} artists={} total={}s",
        lib.track_count(),
        lib.album_count(),
        lib.artist_count(),
        lib.total_duration().as_secs()
    );

    for key in lib.albums() {
        let album = lib.album(key).expect("album for listed key");
        let cover = lib.cover_path(key);
        println!(
            "ALBUM {} — {} ({}) tracks={} art={} cover_cached={}",
            album.artist,
            album.title,
            album.year.map_or("?".into(), |y| y.to_string()),
            album.track_count(),
            album.has_artwork,
            cover.exists()
        );
        for (i, id) in lib.album_tracks(key).iter().take(3).enumerate() {
            let t = lib.track(*id).expect("track for listed id");
            println!(
                "  {} {:?} {} — {}s",
                i + 1,
                t.track_no,
                t.title,
                t.duration.as_secs()
            );
        }
    }

    for artist in lib.artists() {
        println!(
            "ARTIST {} albums={} tracks={}",
            artist.name,
            artist.album_keys.len(),
            artist.track_count
        );
    }

    println!("recently_added: {:?}", lib.recently_added());

    let results = search::search(&lib, "let");
    println!(
        "search 'let': artists={} albums={} tracks={}",
        results.artists.len(),
        results.albums.len(),
        results.tracks.len()
    );
    for id in results.tracks.iter().take(5) {
        let t = lib.track(*id).expect("search hit exists");
        println!("  HIT {} — {}", t.title, t.artist);
    }
}
