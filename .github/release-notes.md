## What's new in v0.2

- A native macOS icon: on macOS 26 the app ships a compiled Icon Composer asset, so the
  Dock no longer shrinks and re-plates it; the fallback `.icns` plate is traced
  pixel-accurate from the system's own icon mask.
- Playlists: drag-and-drop reordering, and **ADD SONGS** moved to the foot of the list.
- Albums: favorite albums get their own **FAVORITES** section on top while keeping their
  place in the full grid.
- Every track list leaves proper breathing room before the scrollbar, the sidebar's accent
  bar no longer touches the window edge, and Settings shows the app version.

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
