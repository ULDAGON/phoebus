# Phoebus × Omarchy

Make Phoebus wear whatever [Omarchy](https://omarchy.org) theme is active — same
surfaces, same accent, switching live — and put a Phoebus widget in the Omarchy bar.

```
contrib/omarchy/install.sh
```

Then add the widget to the bar: put `{"id": "phoebus.media"}` into a section under
`bar.layout` in `~/.config/omarchy/shell.json` and run `omarchy restart shell`.

## What the theming does

Omarchy renders every template in `~/.config/omarchy/themed/` on each theme switch.
`phoebus.toml.tpl` maps the theme's palette (`background`, `foreground`, `accent`, …)
onto Phoebus's own tokens (`bg0`, `text_hi`, `accent`, …), producing
`~/.local/state/omarchy/current/theme/phoebus.toml`. A running Phoebus polls that file
once a second and repaints the moment it changes — switch from Tokyo Night to Rose Pine
and the player follows in the same breath, dark or light mode included.

Details worth knowing:

- **Nothing is overwritten.** The Omarchy palette never touches `state.json`. Remove the
  file (or uninstall the template) and Phoebus falls back to whatever you last picked in
  its Settings.
- **Settings still work.** Picking an accent or mode in Phoebus's Settings while
  following applies on top of the Omarchy surfaces; the next Omarchy theme switch
  re-asserts the full theme. The Settings view says which file it is following.
- `PHOEBUS_THEME` (the one-run override) outranks the file entirely, and
  `PHOEBUS_THEME_FILE=/path/to/file` points the integration at a different file —
  set it empty (`PHOEBUS_THEME_FILE=`) to disable following for a run.
- The file format is documented by the template itself: flat TOML, Phoebus token names,
  `#RRGGBB` values. Any subset of keys is a valid theme.

## What the widget does

`plugin/` is an Omarchy shell bar widget (`phoebus.media`) that appears whenever
Phoebus is running: wordmark dot, play/pause state, current track. Left click toggles
play/pause, right click skips, the wheel steps prev/next, and middle click raises the
Phoebus window (MPRIS `Raise`).

It is optional — Phoebus is a full MPRIS player, so Omarchy's media keys and the
built-in `omarchy.media` widget already control it with nothing installed. This widget
is for keeping Phoebus visible in the bar specifically.

## Uninstall

```
rm ~/.config/omarchy/themed/phoebus.toml.tpl
rm -r ~/.config/omarchy/plugins/phoebus.media
```

(and remove the `phoebus.media` entry from `shell.json` if you added it).
