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
for f in "$bin" "$icns"; do
	[[ -f "$f" ]] || {
		echo "bundle-macos: missing $f" >&2
		exit 1
	}
done

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

# LSMinimumSystemVersion 11.0: the Big Sur icon grid the artwork is drawn on, and the
# oldest macOS the aarch64/x86_64 Rust targets still claim to support.
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
