//! Phoebus — a local music player that looks like Apple Music reskinned by a terminal
//! purist.
//!
//! ```text
//! phoebus                        run normally
//! phoebus --selftest             headless verification, PASS/FAIL lines, exit 0/1
//! phoebus --shot <outdir>        run the UI, walk every view, one PNG per view, quit
//! phoebus --shot-once <path>     run, grab one PNG, quit (see `shots`)
//! ```
//!
//! Environment variables, all optional:
//!
//! | variable | what it does |
//! |----------|--------------|
//! | `PHOEBUS_LIBRARY` | the music to scan; wins over the Settings view's root |
//! | `PHOEBUS_DATA` | the app-data dir (`state.json`, `playlists.json`, `cache/covers/`); default `~/.phoebus/.phoebus` |
//! | `PHOEBUS_THEME` | `dark`/`light`[`,#RRGGBB`] — the palette for one run, never saved |
//! | `PHOEBUS_START_MUTED` | start at volume 0 without persisting it |
//! | `PHOEBUS_SHOT_PLAYING` | `--shot-once` auto-plays first, for a populated player bar |
//! | `PHOEBUS_SELFTEST_EXPECT` | `albums,artists,tracks` minimums for `--selftest` |
//! | `PHOEBUS_MEDIA_LOG` | log every OS media-key event |
//!
//! They are documented at [`phoebus_core::LIBRARY_ENV`], [`phoebus_core::DATA_ENV`],
//! [`theme::ENV_THEME`], [`controller::ENV_START_MUTED`], [`shots::ENV_SHOT_PLAYING`],
//! [`selftest::ENV_EXPECT`] and [`media_keys::ENV_MEDIA_LOG`].

// `deny`, not `forbid`, for exactly one reason: `icon::set_dock_icon` has to call
// `-[NSApplication setApplicationIconImage:]`, and every AppKit setter is an `unsafe fn`
// in objc2. That call carries an `#[allow(unsafe_code)]` and a SAFETY note; it is the only
// one in the workspace, and `forbid` would make the local allow a hard error.
#![deny(unsafe_code)]

mod app;
mod artwork;
mod controller;
mod icon;
mod media_keys;
mod nav;
mod selftest;
mod shots;
mod theme;
mod views;
mod widgets;

use std::path::PathBuf;

use phoebus_core::AppState;

use crate::shots::Capture;

/// What the command line asked for.
enum Mode {
    /// Run the app.
    Run(Option<Capture>),
    /// Run the headless checks and exit.
    Selftest,
}

fn main() -> eframe::Result {
    let mode = parse_args(std::env::args().skip(1));
    // The self-test's output *is* its result, so only warnings are allowed to interleave
    // with it — symphonia in particular is chatty at `info` while decoding.
    let default_level = match mode {
        Mode::Selftest => "warn",
        Mode::Run(_) => "info",
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_level))
        .init();

    let capture = match mode {
        Mode::Selftest => std::process::exit(selftest::run()),
        Mode::Run(capture) => capture,
    };

    // Two independent directories: the data dir Phoebus writes to, and the library root it
    // only ever reads. The data dir has to be resolved first — the configured library root
    // lives in the `state.json` inside it.
    let dirs = phoebus_core::Dirs::resolve();
    if let Err(e) = dirs.ensure_dirs() {
        log::warn!("could not create the app data directory: {e:#}");
    }
    let state = AppState::load_from(&dirs.state_path());
    let root = phoebus_core::library_root_for(state.configured_library_root());
    log::info!(
        "library root: {} (read-only) | app data: {}",
        root.display(),
        dirs.data_dir().display()
    );

    let size = state.window.map_or(theme::WINDOW_DEFAULT, |(w, h)| {
        [w.max(theme::WINDOW_MIN[0]), h.max(theme::WINDOW_MIN[1])]
    });

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Phoebus")
        .with_inner_size(size)
        .with_min_inner_size(theme::WINDOW_MIN)
        .with_app_id("phoebus");
    // `with_icon` takes the data by value, so a PNG that failed to decode means *not
    // calling it* — an icon is never worth refusing to open the window over.
    if let Some(data) = icon::window_icon() {
        viewport = viewport.with_icon(data);
    }

    let options = eframe::NativeOptions {
        viewport: frameless(viewport),
        // Mandatory: ViewportCommand::Screenshot never completes under wgpu on macOS.
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    eframe::run_native(
        "Phoebus",
        options,
        Box::new(move |cc| {
            // The creation closure is the first main-thread callback eframe gives us, and
            // AppKit has finished launching by now — the right moment to replace the
            // generic Dock icon an unbundled binary gets.
            icon::set_dock_icon();
            Ok(Box::new(app::Phoebus::new(cc, dirs, root, state, capture)) as Box<dyn eframe::App>)
        }),
    )?;

    // A `--shot` tour that had to photograph an unsettled step is not a pass: exit
    // non-zero so a script (or a reviewer diffing PNGs) cannot mistake it for one.
    let failed = shots::failed_steps();
    if !failed.is_empty() {
        eprintln!("shot: FAILED steps: {}", failed.join(", "));
        std::process::exit(1);
    }
    Ok(())
}

/// Take the title bar away on macOS (UI-SPEC v1.2 §Window chrome).
///
/// Three settings, all needed: the titlebar goes transparent, the content view grows to
/// fill the whole window behind it, and the title string is hidden. The traffic lights stay
/// — they are the only way to close the window — and float over the sidebar's top-left
/// corner, which is what [`theme::TITLEBAR_PAD`] makes room for.
#[cfg(target_os = "macos")]
fn frameless(viewport: egui::ViewportBuilder) -> egui::ViewportBuilder {
    viewport
        .with_titlebar_shown(false)
        .with_fullsize_content_view(true)
        .with_title_shown(false)
}

/// Linux keeps its normal decorations: the window manager owns that chrome, and a Phoebus
/// window without a title bar there would be a window the user cannot move.
#[cfg(not(target_os = "macos"))]
fn frameless(viewport: egui::ViewportBuilder) -> egui::ViewportBuilder {
    viewport
}

/// Minimal argument parsing. Unknown arguments are logged and ignored so the app still
/// starts; `--selftest` wins over any screenshot flag, and the last screenshot flag wins.
fn parse_args(args: impl Iterator<Item = String>) -> Mode {
    let mut capture = None;
    let mut selftest = false;
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--selftest" => selftest = true,
            "--shot" => match args.next() {
                Some(path) => capture = Some(Capture::Tour(PathBuf::from(path))),
                None => log::warn!("--shot needs an output directory; ignoring it"),
            },
            "--shot-once" => match args.next() {
                Some(path) => capture = Some(Capture::Once(PathBuf::from(path))),
                None => log::warn!("--shot-once needs a path; ignoring it"),
            },
            other => log::warn!("ignoring unknown argument {other:?}"),
        }
    }
    if selftest {
        Mode::Selftest
    } else {
        Mode::Run(capture)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Mode {
        parse_args(args.iter().map(|a| (*a).to_string()))
    }

    fn capture_path(mode: &Mode) -> Option<(&'static str, PathBuf)> {
        match mode {
            Mode::Run(Some(Capture::Once(p))) => Some(("once", p.clone())),
            Mode::Run(Some(Capture::Tour(p))) => Some(("tour", p.clone())),
            _ => None,
        }
    }

    #[test]
    fn args_pick_up_the_screenshot_path() {
        assert_eq!(
            capture_path(&parse(&["--shot-once", "/tmp/a.png"])),
            Some(("once", PathBuf::from("/tmp/a.png")))
        );
        assert_eq!(
            capture_path(&parse(&["--shot", "/tmp/tour"])),
            Some(("tour", PathBuf::from("/tmp/tour")))
        );
        assert!(capture_path(&parse(&[])).is_none());
        assert!(capture_path(&parse(&["--nope"])).is_none());
        assert!(capture_path(&parse(&["--shot-once"])).is_none());
        assert!(capture_path(&parse(&["--shot"])).is_none());
    }

    /// UI-SPEC v1.2 §Window chrome, per platform: macOS gets the frameless look *and* the
    /// sidebar padding that keeps the wordmark clear of the traffic lights; Linux gets
    /// neither, because its decorations are still there.
    #[test]
    fn only_macos_gives_up_its_title_bar() {
        let viewport = frameless(egui::ViewportBuilder::default());
        if cfg!(target_os = "macos") {
            assert_eq!(viewport.titlebar_shown, Some(false));
            assert_eq!(viewport.fullsize_content_view, Some(true));
            assert_eq!(viewport.title_shown, Some(false));
            // The traffic lights need room, and there is no title bar left to give it.
            const { assert!(theme::TITLEBAR_PAD >= 24.0) };
        } else {
            assert_eq!(viewport.titlebar_shown, None);
            assert_eq!(viewport.fullsize_content_view, None);
            assert_eq!(viewport.title_shown, None);
            assert_eq!(theme::TITLEBAR_PAD, 0.0);
        }
    }

    #[test]
    fn selftest_wins_and_needs_no_window() {
        assert!(matches!(parse(&["--selftest"]), Mode::Selftest));
        assert!(matches!(
            parse(&["--shot", "/tmp/x", "--selftest"]),
            Mode::Selftest
        ));
    }
}
