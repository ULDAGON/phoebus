#!/usr/bin/env bash
#
# Wrap the release binary in a minimal macOS application bundle.
#
#   scripts/bundle-macos.sh          build --release first, then bundle
#   scripts/bundle-macos.sh --skip-build
#
# Result: target/bundle/Phoebus.app — double-clickable, shows the real icon in Finder and
# the Dock, and appears under its own name in ⌘-Tab and Control Centre instead of as
# "phoebus" the executable.
#
#   Phoebus.app/Contents/Info.plist
#   Phoebus.app/Contents/MacOS/phoebus          the cargo --release binary, copied
#   Phoebus.app/Contents/Resources/Phoebus.icns assets/icon/Phoebus.icns, copied
#   Phoebus.app/Contents/Resources/Assets.car   assets/icon/Phoebus.icon, compiled here
#
# Two icons because macOS 26 changed what an app icon *is*. Up to macOS 15 the `.icns` is
# the icon: its artwork carries its own rounded plate and the system draws it untouched.
# macOS 26 draws every icon itself — it masks the artwork to the system squircle and adds
# the plate, the shading and the shadow — and an app that offers nothing but an `.icns` is
# assumed to be pre-Tahoe art that already has a plate baked in, so Tahoe shrinks it to
# about ⅔ and centres it on a grey system plate. That is measurable, not folklore: the
# mark spans 408 px of a 1024 px icon that way, against 604 px for a Tahoe-native one.
#
# The fix is to hand Tahoe the artwork *without* a plate and let it do its own compositing,
# which is what `assets/icon/Phoebus.icon` is and what `CFBundleIconName` points at. Both
# keys ship: Tahoe reads `CFBundleIconName` and the compiled `Assets.car`, everything older
# ignores both and reads `CFBundleIconFile` and the `.icns`. See assets/README.md.
#
# The bundle is not signed and not notarised — it is for running the thing you just built,
# not for shipping to other people's machines. `xattr -dr com.apple.quarantine` is not
# needed for a locally built binary; a downloaded one would need it (or a real signature).
#
# Rebuilding overwrites in place. Finder caches icons aggressively, so if the old icon
# lingers after a re-bundle, `touch target/bundle/Phoebus.app` or log out and back in.
#
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
app="$root/target/bundle/Phoebus.app"

[[ "${1-}" == "--skip-build" ]] || (cd "$root" && cargo build --release -p phoebus-app)

bin="$root/target/release/phoebus"
icns="$root/assets/icon/Phoebus.icns"
# A .icon is a document *package*: a directory, hence the separate test.
icon="$root/assets/icon/Phoebus.icon"
for f in "$bin" "$icns"; do
	[[ -f "$f" ]] || {
		echo "bundle-macos: missing $f" >&2
		exit 1
	}
done
[[ -d "$icon" ]] || {
	echo "bundle-macos: missing $icon" >&2
	exit 1
}

# The one version in the tree: [workspace.package] in the root Cargo.toml, which every
# crate inherits with `version.workspace = true`.
version=$(sed -n '/^\[workspace\.package\]/,/^\[/ s/^version *= *"\([^"]*\)".*/\1/p' "$root/Cargo.toml" | head -1)
[[ -n "$version" ]] || {
	echo "bundle-macos: could not read [workspace.package] version from Cargo.toml" >&2
	exit 1
}

rm -rf -- "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp -- "$bin" "$app/Contents/MacOS/phoebus"
cp -- "$icns" "$app/Contents/Resources/Phoebus.icns"

# ---- the macOS 26 icon -------------------------------------------------------------------
# `actool` is Xcode's, not the Command Line Tools' — a machine with only the CLT can still
# produce a working bundle, it just gets Tahoe's shrunken treatment of the `.icns` in the
# Dock. Warn and carry on rather than fail: the alternative is a checked-in `Assets.car`,
# and actool serialises its assets in a hash order that varies run to run, so that file
# could never be committed without churning on every regeneration.
#
# "Carry on" has to be spelled out under `set -e`, and twice over. Finding actool tests that
# it is INSTALLED, not that it is new enough to compile a `.icon`, and an older one does not
# even fail loudly: given a `.icon` it cannot read it exits 0 and writes an empty
# partial.plist with no `Assets.car` beside it. So its exit status is discarded and the
# ARTIFACT is what decides — otherwise either the tool or the `cp` after it aborts the script
# here, before the Info.plist below is written, leaving a directory that is not a bundle at
# all in place of the pre-Tahoe bundle this path is for. The release workflow's
# `test -f …/Assets.car` is the gate that must fail on a release runner, and it can only run
# if this gets that far.
#
# --minimum-deployment-target 26.0 keeps the archive to the renditions Tahoe wants; the
# pre-Tahoe ones are the `.icns` next to it. actool also drops a `Phoebus.icns` of its own
# in the compile directory — a Tahoe render flattened to 16 and 128 pt, the way Apple's own
# apps ship it — which is why it compiles into a temporary directory and only `Assets.car`
# is copied out: the checked-in `.icns` carries 16…1024 drawn on the Big Sur grid, which is
# the better answer for the systems that read it.
icon_name_key=""
if actool_bin=$(xcrun --find actool 2>/dev/null); then
	staging=$(mktemp -d)
	trap 'rm -rf -- "$staging"' EXIT
	"$actool_bin" "$icon" \
		--compile "$staging" \
		--app-icon Phoebus \
		--output-partial-info-plist "$staging/partial.plist" \
		--platform macosx \
		--minimum-deployment-target 26.0 \
		--errors --warnings >/dev/null || true
	if [[ -f "$staging/Assets.car" ]]; then
		cp -- "$staging/Assets.car" "$app/Contents/Resources/Assets.car"
		icon_name_key=$'\t<key>CFBundleIconName</key>\n\t<string>Phoebus</string>'
		echo "bundle-macos: Assets.car compiled from assets/icon/Phoebus.icon"
	else
		echo "bundle-macos: actool compiled no Assets.car — Xcode 26 or newer is needed" \
			"to read a .icon; bundling without the macOS 26 icon, so the Dock will fall" \
			"back to Phoebus.icns and shrink it" >&2
	fi
else
	echo "bundle-macos: no actool (Xcode not installed) — bundling without the macOS 26" \
		"icon; the Dock will fall back to Phoebus.icns and shrink it" >&2
fi

# LSMinimumSystemVersion 11.0: the Big Sur icon grid the `.icns` artwork is drawn on, and
# the oldest macOS the aarch64/x86_64 Rust targets still claim to support.
cat >"$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>Phoebus</string>
	<key>CFBundleDisplayName</key>
	<string>Phoebus</string>
	<key>CFBundleIdentifier</key>
	<string>dev.phoebus.player</string>
	<key>CFBundleExecutable</key>
	<string>phoebus</string>
	<key>CFBundleIconFile</key>
	<string>Phoebus</string>
$icon_name_key
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleShortVersionString</key>
	<string>$version</string>
	<key>CFBundleVersion</key>
	<string>$version</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.music</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSSupportsAutomaticGraphicsSwitching</key>
	<true/>
</dict>
</plist>
PLIST

# Nudge Launch Services so Finder picks the icon up on the first look.
touch -- "$app"
echo "bundle-macos: $app"
