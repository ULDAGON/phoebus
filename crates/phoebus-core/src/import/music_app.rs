//! [`Mode::MusicApp`](super::Mode::MusicApp): the Music app files the song, Phoebus asks.
//!
//! All AppleScript in Phoebus lives in this module. It is three fixed scripts — [`ADD`],
//! [`SET_FIELDS`], [`SET_ARTWORK`] — run by `/usr/bin/osascript`, and the one rule that
//! matters is that **nothing is ever spliced into a script**. Every path and every value
//! travels as an `osascript` argument and arrives in the script as `item N of argv`, so a
//! song called `"); do shell script "rm -rf ~` is a title like any other. The scripts are
//! `const` strings; [`Invocation`] can only put arguments after them.
//!
//! Each script answers with the track's persistent ID and the POSIX path of its file, one
//! per line ([`parse_reply`]). The path matters because the Music app moves files: it
//! copies a new song into `Media/Music/<Artist>/<Album>/`, and with "Keep Music Media folder
//! organised" on it moves it again whenever an edit changes the artist or the album.
//!
//! The first script run triggers macOS's automation consent ("Phoebus wants to control
//! Music"). Until the user answers, `osascript` blocks; a refusal comes back as error
//! -1743 and is reported as such ([`reason`]).

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{Failure, Fields, ImageKind};

/// `osascript`'s fixed location on every macOS.
const OSASCRIPT: &str = "/usr/bin/osascript";

/// How long one script may take. Generous, because the first one may launch the Music app
/// and then wait on the consent dialog for as long as the user takes to read it.
const TIMEOUT: Duration = Duration::from_secs(180);

/// Poll interval while waiting for `osascript`.
const POLL: Duration = Duration::from_millis(25);

/// The error number [`ADD`] raises when the Music app returns no track.
const ERR_REFUSED: i32 = 9001;
/// The error number the edit scripts raise when the persistent ID is no longer a track.
const ERR_GONE: i32 = 9002;

/// Add one file to the Music library. `argv`: the file's POSIX path.
///
/// `add` hands back a track, or a list of tracks when the app decides the file is several,
/// or `missing value` when it refused the file without saying why — what it does with a
/// format it cannot play.
pub const ADD: &str = r#"on run argv
	set src to POSIX file (item 1 of argv)
	tell application "Music"
		set t to add src
		if class of t is list then
			if t is {} then error "the Music app added nothing" number 9001
			set t to item 1 of t
		end if
		if t is missing value then error "the Music app added nothing" number 9001
		set pid to persistent ID of t
		set loc to location of t
	end tell
	if loc is missing value then return pid & linefeed
	return pid & linefeed & (POSIX path of loc)
end run"#;

/// Write the editable fields of one track. `argv`: persistent ID, title, artist, album,
/// album artist, genre, year (`0` = none), track number (`0` = none).
pub const SET_FIELDS: &str = r#"on run argv
	set pid to item 1 of argv
	tell application "Music"
		set hits to (every track of library playlist 1 whose persistent ID is pid)
		if hits is {} then error "no such track in the Music library" number 9002
		set t to item 1 of hits
		set name of t to item 2 of argv
		set artist of t to item 3 of argv
		set album of t to item 4 of argv
		set album artist of t to item 5 of argv
		set genre of t to item 6 of argv
		set year of t to ((item 7 of argv) as integer)
		set track number of t to ((item 8 of argv) as integer)
		set loc to location of t
	end tell
	if loc is missing value then return pid & linefeed
	return pid & linefeed & (POSIX path of loc)
end run"#;

/// Set the artwork of one track from an image file. `argv`: persistent ID, the image's
/// POSIX path, and `png` or `jpeg`.
///
/// The image is read *outside* the `tell`: file reading is a scripting addition of the
/// script's own process, and the Music app should only ever receive picture data.
pub const SET_ARTWORK: &str = r#"on run argv
	set pid to item 1 of argv
	set f to open for access (POSIX file (item 2 of argv))
	try
		if item 3 of argv is "png" then
			set pic to read f as «class PNGf»
		else
			set pic to read f as «class JPEG»
		end if
	on error m number n
		close access f
		error m number n
	end try
	close access f
	tell application "Music"
		set hits to (every track of library playlist 1 whose persistent ID is pid)
		if hits is {} then error "no such track in the Music library" number 9002
		set t to item 1 of hits
		set data of artwork 1 of t to pic
		set loc to location of t
	end tell
	if loc is missing value then return pid & linefeed
	return pid & linefeed & (POSIX path of loc)
end run"#;

/// What the Music app reports about a track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MusicTrack {
    /// The track's persistent ID: 16 hex digits, stable for the life of the library.
    pub persistent_id: String,
    /// The file, as the Music app has filed it — `None` while it has not finished copying.
    pub location: Option<PathBuf>,
}

/// One `osascript` run: a fixed script and the arguments it reads from `argv`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    script: &'static str,
    args: Vec<OsString>,
}

impl Invocation {
    /// The full argument vector for `osascript`: each script line as its own `-e`, then
    /// `--`, then the arguments, untouched.
    ///
    /// The `--` ends option parsing, so an argument that happens to start with `-` is
    /// still an argument and not an `osascript` flag.
    pub fn argv(&self) -> Vec<OsString> {
        let mut out: Vec<OsString> = Vec::new();
        for line in self.script.lines() {
            out.push("-e".into());
            out.push(line.into());
        }
        out.push("--".into());
        out.extend(self.args.iter().cloned());
        out
    }
}

/// The [`ADD`] run for `src`.
pub fn add_invocation(src: &Path) -> Invocation {
    Invocation {
        script: ADD,
        args: vec![src.as_os_str().to_owned()],
    }
}

/// The [`SET_FIELDS`] run for the track `id`.
pub fn fields_invocation(id: &str, fields: &Fields) -> Invocation {
    let number = |n: Option<u32>| n.unwrap_or(0).to_string();
    Invocation {
        script: SET_FIELDS,
        args: [
            id.to_string(),
            fields.title.trim().to_string(),
            fields.artist.trim().to_string(),
            fields.album.trim().to_string(),
            fields.album_artist.trim().to_string(),
            fields.genre.trim().to_string(),
            number(fields.year),
            number(fields.track_no),
        ]
        .into_iter()
        .map(OsString::from)
        .collect(),
    }
}

/// The [`SET_ARTWORK`] run for the track `id` and the image at `image`.
pub fn artwork_invocation(id: &str, image: &Path, kind: ImageKind) -> Invocation {
    let kind = match kind {
        ImageKind::Png => "png",
        ImageKind::Jpeg => "jpeg",
    };
    Invocation {
        script: SET_ARTWORK,
        args: vec![id.into(), image.as_os_str().to_owned(), kind.into()],
    }
}

/// True for the formats the Music app adds: AAC/ALAC in `.m4a`, MP3, WAV, AIFF. It
/// plays neither FLAC nor Ogg Vorbis, and `add` drops them without a word.
pub fn accepts(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    matches!(ext.as_str(), "m4a" | "mp3" | "wav" | "aiff" | "aif" | "aac")
}

/// True if `s` looks like a persistent ID: 16 hex digits.
pub fn is_persistent_id(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parse what the scripts print: the persistent ID, a line feed, and the POSIX path (or
/// nothing). Only the first line feed splits, so a path with one in it survives; the one
/// line feed `osascript` appends is dropped.
pub fn parse_reply(stdout: &str) -> Result<MusicTrack, Failure> {
    let out = stdout.strip_suffix('\n').unwrap_or(stdout);
    let (id, path) = out.split_once('\n').unwrap_or((out, ""));
    let id = id.trim();
    if !is_persistent_id(id) {
        return Err(Failure::new(
            "UNEXPECTED MUSIC APP REPLY",
            format!("{stdout:?} is not a persistent ID and a path"),
        ));
    }
    Ok(MusicTrack {
        persistent_id: id.to_ascii_uppercase(),
        location: (path.starts_with('/')).then(|| PathBuf::from(path)),
    })
}

/// The AppleScript error number at the end of an `osascript` error line, e.g. the `-1743`
/// of `0:57: execution error: Not authorized to send Apple events to Music. (-1743)`.
pub fn error_number(stderr: &str) -> Option<i32> {
    let line = stderr.trim_end();
    let open = line.rfind('(')?;
    line[open + 1..].strip_suffix(')')?.parse().ok()
}

/// Turn an `osascript` failure into the short status the import list shows.
pub fn reason(stderr: &str) -> String {
    match error_number(stderr) {
        Some(-1743) => "NOT ALLOWED TO CONTROL MUSIC",
        Some(-600 | -609 | -903) => "MUSIC APP NOT AVAILABLE",
        Some(-1712) => "MUSIC APP DID NOT ANSWER",
        Some(-43) => "FILE NOT FOUND",
        Some(-54 | -61 | -5000) => "FILE NOT ACCESSIBLE",
        Some(ERR_GONE | -1728) => "NOT IN THE MUSIC LIBRARY",
        Some(ERR_REFUSED) => "MUSIC APP REFUSED",
        _ => "MUSIC APP REFUSED",
    }
    .to_string()
}

/// Run one invocation and parse its reply.
pub fn run(invocation: &Invocation) -> Result<MusicTrack, Failure> {
    let mut child = Command::new(OSASCRIPT)
        .args(invocation.argv())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Failure::new("OSASCRIPT NOT AVAILABLE", e.to_string()))?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Failure::new(
                    "MUSIC APP DID NOT ANSWER",
                    format!("osascript ran for more than {} s", TIMEOUT.as_secs()),
                ));
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => return Err(Failure::new("OSASCRIPT FAILED", e.to_string())),
        }
    };
    // The replies are a line or two, far below a pipe's buffer, so reading after the exit
    // cannot deadlock.
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    if !status.success() {
        log::warn!("music app: osascript failed: {}", stderr.trim());
        return Err(Failure::new(reason(&stderr), stderr.trim().to_string()));
    }
    parse_reply(&stdout)
}

/// Add `src` to the Music library.
pub fn add(src: &Path) -> Result<MusicTrack, Failure> {
    run(&add_invocation(src))
}

/// Write `fields` to the track `id`.
pub fn set_fields(id: &str, fields: &Fields) -> Result<MusicTrack, Failure> {
    run(&fields_invocation(id, fields))
}

/// Set the artwork of the track `id` from the image at `image`.
pub fn set_artwork(id: &str, image: &Path, kind: ImageKind) -> Result<MusicTrack, Failure> {
    run(&artwork_invocation(id, image, kind))
}

#[cfg(test)]
mod tests {
    use super::super::tests::sample;
    use super::*;

    /// Split an argv back into (script, arguments) the way `osascript` reads it.
    fn split(argv: &[OsString]) -> (String, Vec<OsString>) {
        let mut lines = Vec::new();
        let mut i = 0;
        while i < argv.len() && argv[i] == "-e" {
            lines.push(argv[i + 1].to_string_lossy().into_owned());
            i += 2;
        }
        assert_eq!(
            argv[i], "--",
            "the arguments are fenced off from the options"
        );
        (lines.join("\n"), argv[i + 1..].to_vec())
    }

    #[test]
    fn hostile_names_travel_as_arguments_and_never_touch_the_script() {
        let evil = r#"/tmp/"); do shell script "rm -rf ~" --
-x.m4a"#;
        let argv = add_invocation(Path::new(evil)).argv();
        let (script, args) = split(&argv);
        assert_eq!(script, ADD, "the script is the constant, byte for byte");
        assert_eq!(args, vec![OsString::from(evil)]);

        let fields = Fields {
            title: r#"Say "Hi" & \ bye"#.into(),
            artist: "-n".into(),
            ..sample()
        };
        let argv = fields_invocation("0123456789ABCDEF", &fields).argv();
        let (script, args) = split(&argv);
        assert_eq!(script, SET_FIELDS);
        let args: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "0123456789ABCDEF",
                r#"Say "Hi" & \ bye"#,
                "-n",
                "Odyssey",
                "HOME",
                "Synthwave",
                "2014",
                "3"
            ]
        );
    }

    #[test]
    fn missing_numbers_are_sent_as_zero_which_clears_them_in_music() {
        let fields = Fields {
            year: None,
            track_no: None,
            ..sample()
        };
        let argv = fields_invocation("0123456789ABCDEF", &fields).argv();
        let (_, args) = split(&argv);
        assert_eq!(args[6], "0");
        assert_eq!(args[7], "0");
    }

    #[test]
    fn the_artwork_run_names_its_format() {
        let argv =
            artwork_invocation("0123456789ABCDEF", Path::new("/a/b c.jpg"), ImageKind::Jpeg).argv();
        let (script, args) = split(&argv);
        assert_eq!(script, SET_ARTWORK);
        assert_eq!(args, ["0123456789ABCDEF", "/a/b c.jpg", "jpeg"]);
    }

    /// Every script reads only from `argv`, and reads exactly the items its builder sends:
    /// an off-by-one here would set the album artist from the genre.
    #[test]
    fn each_script_reads_exactly_the_items_its_builder_sends() {
        for (script, count) in [(ADD, 1), (SET_FIELDS, 8), (SET_ARTWORK, 3)] {
            assert!(script.starts_with("on run argv") && script.ends_with("end run"));
            for n in 1..=count {
                assert!(script.contains(&format!("item {n} of argv")), "item {n}");
            }
            assert!(!script.contains(&format!("item {} of argv", count + 1)));
        }
        assert_eq!(add_invocation(Path::new("/x")).args.len(), 1);
        assert_eq!(fields_invocation("x", &sample()).args.len(), 8);
        assert_eq!(
            artwork_invocation("x", Path::new("/x"), ImageKind::Png)
                .args
                .len(),
            3
        );
        // The error numbers the scripts raise are the ones `reason` maps.
        assert!(ADD.contains(&format!("number {ERR_REFUSED}")));
        assert!(SET_FIELDS.contains(&format!("number {ERR_GONE}")));
        assert!(SET_ARTWORK.contains(&format!("number {ERR_GONE}")));
    }

    #[test]
    fn replies_parse_into_an_id_and_a_location() {
        let ok = parse_reply("9A0B1C2D3E4F5A6B\n/Users/me/Music/Music/Media.localized/Music/HOME/Odyssey/03 Resonance.m4a\n")
            .expect("parsed");
        assert_eq!(ok.persistent_id, "9A0B1C2D3E4F5A6B");
        assert_eq!(
            ok.location.as_deref(),
            Some(Path::new(
                "/Users/me/Music/Music/Media.localized/Music/HOME/Odyssey/03 Resonance.m4a"
            ))
        );
        let lower = parse_reply("9a0b1c2d3e4f5a6b\n\n").expect("parsed");
        assert_eq!(lower.persistent_id, "9A0B1C2D3E4F5A6B");
        assert_eq!(lower.location, None, "still copying: no location yet");
        let odd = parse_reply("9A0B1C2D3E4F5A6B\n/a/line\nbreak.m4a\n").expect("parsed");
        assert_eq!(
            odd.location.as_deref(),
            Some(Path::new("/a/line\nbreak.m4a"))
        );

        for bad in [
            "",
            "\n",
            "missing value\n",
            "123\n/x",
            "9A0B1C2D3E4F5A6G\n/x",
        ] {
            let err = parse_reply(bad).expect_err(bad);
            assert_eq!(err.reason, "UNEXPECTED MUSIC APP REPLY");
        }
    }

    #[test]
    fn osascript_errors_become_short_reasons() {
        let denied =
            "0:57: execution error: Not authorized to send Apple events to Music. (-1743)\n";
        assert_eq!(error_number(denied), Some(-1743));
        assert_eq!(reason(denied), "NOT ALLOWED TO CONTROL MUSIC");
        assert_eq!(
            reason("12:40: execution error: the Music app added nothing (9001)"),
            "MUSIC APP REFUSED"
        );
        assert_eq!(
            reason("execution error: no such track in the Music library (9002)"),
            "NOT IN THE MUSIC LIBRARY"
        );
        assert_eq!(
            reason("execution error: Music got an error: Application isn't running. (-600)"),
            "MUSIC APP NOT AVAILABLE"
        );
        assert_eq!(reason("garbage without a number"), "MUSIC APP REFUSED");
        assert_eq!(error_number("(abc)"), None);
    }

    #[test]
    fn only_formats_the_music_app_plays_are_offered_to_it() {
        for yes in ["a.m4a", "a.MP3", "a.wav", "a.aiff", "a.aif", "a.aac"] {
            assert!(accepts(Path::new(yes)), "{yes}");
        }
        for no in ["a.flac", "a.ogg", "a", "a.m4a.part"] {
            assert!(!accepts(Path::new(no)), "{no}");
        }
    }

    /// The scripts are real AppleScript: compile all three with `osacompile`, which
    /// parses without running anything — no event is sent, the Music app is not launched.
    /// macOS only, where the compiler exists.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_scripts_compile() {
        let dir = tempfile::tempdir().expect("tempdir");
        for (name, script) in [
            ("add", ADD),
            ("fields", SET_FIELDS),
            ("artwork", SET_ARTWORK),
        ] {
            let out = dir.path().join(format!("{name}.scpt"));
            let status = Command::new("/usr/bin/osacompile")
                .arg("-o")
                .arg(&out)
                .arg("-e")
                .arg(script)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .expect("osacompile runs");
            assert!(
                status.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&status.stderr)
            );
        }
    }
}
