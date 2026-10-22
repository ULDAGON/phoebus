## What's new in v0.4.0

- **Instant launch.** Phoebus saves the library after each scan and shows it at once on
  the next launch, while a background scan reads only the files that changed. On a library
  of 3,400 songs the library is on screen in about 0.25 s instead of about 4 s.
- **Every song shows its own cover.** Song lists, Up Next, the player bar and Now Playing
  show the art of each song, not of its album. An album whose songs carry different art,
  such as a compilation, shows a four-tile mosaic. The first launch after the update scans
  the library once to build the new cover cache.
- **Import in Settings.** Drop songs or album folders anywhere on the window. On macOS with
  an Apple Music library, Phoebus adds them through the Music app, exactly like a drop into
  Apple Music; edit the fields or the cover right after the import. macOS asks once for
  permission to control Music. Elsewhere, Phoebus copies the files into the library as
  `Artist/Album/NN Title`.

## Install

### macOS
1. Download `Phoebus-macos-universal.zip` and unzip it.
2. Move `Phoebus.app` to `/Applications` (optional) and open it.
3. The app is not notarised, so the first launch is blocked by Gatekeeper. Either right-click the app and choose **Open**, or run:
   ```
   xattr -dr com.apple.quarantine /Applications/Phoebus.app
   ```

### Linux
1. Download and unpack `phoebus-linux-x86_64.tar.gz`.
2. Run `./phoebus` — that's it.
3. Optional desktop integration: copy `phoebus.desktop` to `~/.local/share/applications/` and the `icons/hicolor` tree to `~/.local/share/icons/`, with the `phoebus` binary somewhere on your `PATH`.
4. On Omarchy: clone the repo and run `contrib/omarchy/install.sh` for live theming and the bar widget (see `contrib/omarchy/README.md`).
