## What's new in v0.3.0

- **Omarchy theming.** On Linux, Phoebus now follows the [Omarchy](https://omarchy.org)
  desktop theme live — surfaces, accent, dark/light — switching in the same second as
  the desktop. Run `contrib/omarchy/install.sh` to set up the bridge.
- **A theme source toggle.** While a desktop theme is on offer, Settings → THEME grows
  a `SOURCE` pair: `DESKTOP` (the default) follows it, `STOCK` keeps Phoebus's own
  dark-blue-and-yellow look. The choice persists.
- **An Omarchy bar widget.** `phoebus.media` shows a play glyph and the current track
  whenever Phoebus runs. Left click opens a now-playing panel: cover art, seek bar,
  transport buttons, a shuffle toggle, and a collapsible up-next list — click a row to
  jump to it.
- **A queue service.** Phoebus serves its up-next queue over D-Bus
  (`org.phoebus.Queue`) for desktop widgets — MPRIS has no queue. Linux only.

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
