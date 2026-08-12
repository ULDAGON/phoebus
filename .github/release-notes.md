## What's new in v0.2.1

- On macOS, closing the window with the red button now keeps Phoebus alive in the
  background. Press **Command+Q** when you want to quit the app completely.
- The app icon now gives the play symbol more breathing room and uses a near-black
  background with a subtle blue tint across macOS and Linux.

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
