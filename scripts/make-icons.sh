#!/usr/bin/env bash
#
# Regenerate every raster icon from the two SVGs in assets/icon/.
#
#   scripts/make-icons.sh
#
# Inputs   assets/icon/phoebus.svg          the macOS plate (824 body in a 1024 canvas)
#          assets/icon/phoebus-square.svg   full-bleed, for Linux and the window icon
#
# Outputs  assets/icon/Phoebus.icns         macOS bundle icon up to macOS 15
#                                           (10 iconutil entries, 16..1024)
#          assets/icon/hicolor/<N>x<N>/apps/phoebus.png   Linux hicolor set
#          assets/icon/phoebus-256.png      the plate PNG compiled into the binary
#                                           (window icon everywhere, Dock icon on macOS)
#          assets/icon/Phoebus.icon/Assets/{field,mark}-grain.png
#                                           the grain layers of the macOS 26 icon
#
# All of them are committed, so building Phoebus from source needs none of the tools below —
# this script only has to run when the SVGs change.
#
# The rest of the macOS 26 icon is not here. Tahoe wants artwork with no plate on it and
# composites the plate itself, so it gets its own source — assets/icon/Phoebus.icon, its
# icon.json and mark.svg hand-written — which scripts/bundle-macos.sh compiles with actool
# at bundle time. actool's output cannot be committed: it orders its assets by a hash that
# moves between runs.
#
# Requires rsvg-convert (brew install librsvg) plus iconutil, which ships with macOS.
# Without a Mac you can still refresh everything but the .icns. Output is deterministic:
# running it twice leaves every file byte-identical, so a no-op run shows up as an empty
# `git diff`.
#
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
icons="$root/assets/icon"
plate="$icons/phoebus.svg"
square="$icons/phoebus-square.svg"

need() {
	command -v "$1" >/dev/null 2>&1 || {
		echo "make-icons: need $1 — $2" >&2
		exit 1
	}
}
need rsvg-convert "brew install librsvg"

# rsvg-convert writes a bare RGBA PNG with no timestamp chunk, so this is reproducible.
render() { # render <svg> <px> <out>
	rsvg-convert -w "$2" -h "$2" "$1" -o "$3"
}

# ---- macOS 11..15: Phoebus.icns ----------------------------------------------------------
# The plate artwork goes in whole, because these systems draw an app icon exactly as the
# .icns gives it — the rounded square has to be in the pixels. macOS 26 reads Assets.car
# instead (see the note above and scripts/bundle-macos.sh) and ignores this file.
#
# iconutil only accepts the classic names, and the @2x file of one entry is the same pixel
# count as the 1x file of the next: 16/32, 32/64, 128/256, 256/512, 512/1024.
if command -v iconutil >/dev/null 2>&1; then
	set="$(mktemp -d)/Phoebus.iconset"
	mkdir -p "$set"
	trap 'rm -rf -- "$(dirname -- "$set")"' EXIT
	for pair in 16:32 32:64 128:256 256:512 512:1024; do
		pt=${pair%%:*}
		px=${pair##*:}
		render "$plate" "$pt" "$set/icon_${pt}x${pt}.png"
		render "$plate" "$px" "$set/icon_${pt}x${pt}@2x.png"
	done
	iconutil -c icns "$set" -o "$icons/Phoebus.icns"
	echo "make-icons: $icons/Phoebus.icns ($(wc -c <"$icons/Phoebus.icns" | tr -d ' ') bytes)"
else
	echo "make-icons: iconutil not found (not macOS) — skipping Phoebus.icns" >&2
fi

# ---- Linux: hicolor theme ---------------------------------------------------------------
# Installed by copying assets/icon/hicolor/ over ~/.local/share/icons/hicolor/ — see
# assets/README.md.
for px in 32 48 64 128 256 512; do
	dir="$icons/hicolor/${px}x${px}/apps"
	mkdir -p "$dir"
	render "$square" "$px" "$dir/phoebus.png"
done
echo "make-icons: hicolor 32,48,64,128,256,512 -> $icons/hicolor"

# ---- The window icon compiled into the binary -------------------------------------------
# crates/phoebus-app/src/icon.rs does `include_bytes!` on this one, hands the decoded RGBA
# to egui's ViewportBuilder::with_icon, and on macOS hands the PNG itself to AppKit for the
# Dock. It is the *plate*, not the square: nothing masks a macOS Dock icon, so a full-bleed
# square would sit in the Dock as a hard-edged tile.
render "$plate" 256 "$icons/phoebus-256.png"
echo "make-icons: $icons/phoebus-256.png ($(wc -c <"$icons/phoebus-256.png" | tr -d ' ') bytes)"

# ---- macOS 26: the grain layers of Phoebus.icon -----------------------------------------
# icon.json has no noise of its own, so the grain goes in as two PNG layers that icon.json
# blends with `overlay` at 0.6. They come from phoebus-square.svg because the .icon canvas
# is full-bleed too. The stylesheet hides everything but the one group and drops its
# opacity and blend mode, which would otherwise be applied twice. (`--export-id` would be
# the obvious tool, but librsvg 2.63 renders a filtered element empty with it.)
for layer in field-grain mark-grain; do
	rsvg-convert -w 1024 -h 1024 -s <(
		echo "svg > :not(defs, #$layer) { display: none }"
		echo "#$layer { opacity: 1 !important; mix-blend-mode: normal !important }"
	) "$square" -o "$icons/Phoebus.icon/Assets/$layer.png"
done
echo "make-icons: field-grain, mark-grain -> $icons/Phoebus.icon/Assets"
