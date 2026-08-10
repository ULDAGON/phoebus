# Packaging assets

Everything an installed Phoebus needs that is not the binary — plus the one file the binary
needs itself: `icon/phoebus-256.png` is `include_bytes!`-ed by
`crates/phoebus-app/src/icon.rs`, so deleting it breaks the build. Everything else here is
packaging, and every generated file is checked in, so a build from source needs no tools
beyond cargo.

```
icon/phoebus.svg              the source artwork: Big Sur plate, 824 body in a 1024 canvas
icon/phoebus-square.svg       the same mark full-bleed, no plate, no margin
icon/reference.jpg            the raster both were measured off, kept as the design reference
icon/Phoebus.icns             generated — the macOS bundle icon, 16…1024 with @2x pairs
icon/hicolor/<N>x<N>/apps/    generated — the Linux icon theme set, 32/48/64/128/256/512
icon/phoebus-256.png          generated — compiled into the binary (window + Dock icon)
linux/phoebus.desktop         the launcher entry template
```

The three generated groups come from `scripts/make-icons.sh`, which needs `rsvg-convert`
(`brew install librsvg` / `apt install librsvg2-bin`) plus macOS's `iconutil` for the
`.icns`. They are committed so nobody building from source has to install any of that —
re-run the script only when an SVG changes. On one machine, one librsvg, it is
byte-reproducible: re-running it over unchanged SVGs leaves an empty `git diff`. Across
librsvg (or libpng, or Cairo) versions that is not promised — identical pixels can be
re-encoded differently — so a diff right after a toolchain upgrade is not by itself
evidence the artwork moved.

## macOS

`scripts/bundle-macos.sh` builds `--release` and wraps the binary in
`target/bundle/Phoebus.app`, with `Phoebus.icns` in `Contents/Resources` and an
`Info.plist` naming `dev.phoebus.player`. Drag it to `/Applications` if you want it there.

Run unbundled (`cargo run`, `./target/release/phoebus`) and there is no `Info.plist` for
AppKit to read, so `crates/phoebus-app/src/icon.rs` sets the Dock icon at runtime instead —
the same artwork, no bundle required.

## Linux

Copy the theme set and the launcher entry into the per-user data dirs:

```sh
cp -r assets/icon/hicolor  ~/.local/share/icons/
install -Dm644 assets/linux/phoebus.desktop ~/.local/share/applications/phoebus.desktop
gtk-update-icon-cache -f -t ~/.local/share/icons/hicolor 2>/dev/null || true
update-desktop-database ~/.local/share/applications 2>/dev/null || true
```

System-wide, use `/usr/share/icons/hicolor` and `/usr/share/applications` instead. The
entry's `Exec=phoebus` expects the binary on `$PATH`; point it at
`/path/to/target/release/phoebus` if it is not. `Icon=phoebus` and `StartupWMClass=phoebus`
both match the app id the window sets (`main.rs`, `with_app_id("phoebus")`), which is what
lets the shell attach a running window to the launcher entry.

## The artwork

Two colours, sampled from `icon/reference.jpg`: navy `#2A3440` and neon yellow `#FEFB54`.
The yellow is flat wherever it appears. The navy is not: both SVGs fill their background
with a vertical gradient from `#303B49` down to `#2A3440` — a few per cent, just enough
that 1024 px of one colour does not read as dead — and the plate variant adds a hairline
rim light along its top edge, white at 13 % opacity fading to nothing by mid-height, a 4 px
stroke clipped to the plate so only its inner half shows. The full-bleed square has the
gradient and no rim; it has no edge for a light to catch. The yellow is deliberately *not*
the app's `#FFFB00` accent — the icon is the brand mark, the accent is a UI token, and they
are allowed to differ.

Neither SVG is a trace of the reference. Every number below was measured off it and the
shapes re-drawn from those measurements, which is why the geometry can be stated as exact
fractions at all.

Geometry, as fractions of the plate (or of the whole canvas, for the full-bleed variant):
ring outer diameter 86.2 % with a 13.5 % stroke; the play triangle equilateral with a
32.63 % circumradius and 2.95 % corner radius, its centroid nudged 1.28 % left of the ring
centre. That last nudge is the reference's, and it is what puts the apex just short of the
ring while the two left corners kiss it.

The plate outline is a superellipse `|x/412|^5 + |y/412|^5 = 1`, not a rounded rectangle:
Apple's corner is continuous-curvature and an `rx` rect reads visibly rounder beside stock
icons. Four cubics per quadrant track it to within 0.14 px at 1024.
