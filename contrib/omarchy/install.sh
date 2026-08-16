#!/bin/bash
# Wire Phoebus into Omarchy: theme bridge + bar widget.
#
# Copies the bridge template into ~/.config/omarchy/themed/ (rendered by Omarchy on
# every theme switch into ~/.local/state/omarchy/current/theme/phoebus.toml, which a
# running Phoebus follows live) and the bar widget plugin into
# ~/.config/omarchy/plugins/. Both are plain copies — rerun after updating Phoebus,
# remove the copied files to uninstall.

set -euo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

mkdir -p "$HOME/.config/omarchy/themed"
cp "$SRC/phoebus.toml.tpl" "$HOME/.config/omarchy/themed/"
echo "Installed theme template -> ~/.config/omarchy/themed/phoebus.toml.tpl"

mkdir -p "$HOME/.config/omarchy/plugins/phoebus.media"
cp "$SRC/plugin/manifest.json" "$SRC/plugin/BarWidget.qml" "$HOME/.config/omarchy/plugins/phoebus.media/"
echo "Installed bar widget      -> ~/.config/omarchy/plugins/phoebus.media/"

# Render phoebus.toml for the theme already in force, so Phoebus recolours now
# rather than on the next theme switch.
if command -v omarchy-theme-refresh >/dev/null 2>&1; then
  omarchy-theme-refresh
  echo "Rendered the current theme's phoebus.toml"
else
  echo "omarchy-theme-refresh not found: the file appears on the next theme switch"
fi

echo
echo "Done. To show the widget, add  {\"id\": \"phoebus.media\"}  to a bar section"
echo "of ~/.config/omarchy/shell.json (then: omarchy restart shell)."
