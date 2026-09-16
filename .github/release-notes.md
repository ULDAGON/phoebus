## What's new in v0.3.1

- **Output device changes no longer silence playback.** Unplugging the earphones, or
  picking another output in Control Centre, used to leave the player "playing" with
  no sound, and the app could not quit afterwards. Phoebus now follows the system's
  default output device: playback moves to it within a second and keeps its position
  and play/pause state.
- **Shell-escaped paths work in Settings.** A library path pasted from a terminal
  (`Mobile\ Documents/com\~apple\~CloudDocs`) is accepted as the same directory as its
  plain spelling.

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
