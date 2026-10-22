## What's new in v0.3.2

- **A new app icon.** A thinner ring, a lighter plate that lightens towards the top, a
  warmer yellow, and a fine grain on both. On macOS 26 it is drawn by the system like
  the built-in apps.
- **Albums and playlists open at the top.** A newly opened album could show the middle
  of its tracklist, with the `← ALBUMS` button scrolled out of view. Now every album
  and playlist opens at the top, and Back returns to the position you left.

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
