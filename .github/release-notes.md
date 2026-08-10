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
