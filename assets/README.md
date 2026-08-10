# Packaging assets

Everything an installed Phoebus needs that is not the binary — plus the one file the binary
needs itself: `icon/phoebus-256.png` is `include_bytes!`-ed by
`crates/phoebus-app/src/icon.rs`, so deleting it breaks the build. Everything else here is
packaging, and every generated file is checked in, so a build from source needs no tools
beyond cargo.

```
icon/phoebus.svg              the source artwork: Big Sur plate, 824 body in a 1024 canvas
icon/phoebus-square.svg       the same mark full-bleed, no plate, no margin
icon/Phoebus.icon/            the source artwork again, as macOS 26 wants it: icon.json
                              plus Assets/mark.svg, the mark on nothing
icon/reference.jpg            the raster they were measured off, kept as the design reference
icon/Phoebus.icns             generated — the macOS 11…15 bundle icon, 16…1024 with @2x pairs
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
`target/bundle/Phoebus.app`, with an `Info.plist` naming `dev.phoebus.player` and **two**
icons in `Contents/Resources` — `Phoebus.icns` and a compiled `Assets.car`. Drag it to
`/Applications` if you want it there.

Two icons because macOS 26 changed what an app icon is. Up to macOS 15 the `.icns` *is* the
icon: the rounded plate is part of the artwork and the system draws the file untouched.
macOS 26 composites every icon itself — it masks the artwork to the system squircle and
adds the plate, the shading and the shadow — and reads an app that offers only an `.icns`
as pre-Tahoe art with a plate already baked in, so it shrinks that art to about two thirds
and centres it on a grey system plate of its own. The result is a small dark tile floating
inside a bigger pale one, which is exactly what it looks like.

The numbers, measured off what `NSWorkspace` hands back for a registered bundle, rendered
at 1024: with only an `.icns`, the yellow mark spans 408 px. With `Assets.car`, 604 px —
which is the size it is drawn at, and the same fraction of the plate that Music.app and
Podcasts.app give their glyphs. Both put the plate at 824 px of 1024, the macOS grid.

So the `.icon` carries the mark on a flat navy field and no plate at all, and lets Tahoe do
the compositing. `scripts/bundle-macos.sh` compiles it with `actool`, which ships inside
Xcode; a machine with only the Command Line Tools still gets a bundle, just one that
Tahoe's Dock shrinks. `Assets.car` is not committed like the other generated files because
it cannot be: `actool` serialises the archive in an order that varies between runs, so the
file differs byte for byte every time even when nothing changed.

The two `Info.plist` keys are how the split works. `CFBundleIconFile` names the `.icns`,
`CFBundleIconName` names the asset inside `Assets.car`; macOS 26 prefers the second and
everything older has never heard of it. Apple's own apps ship exactly this pair — see
`Music.app/Contents/Info.plist`.

Run unbundled (`cargo run`, `./target/release/phoebus`) and there is no `Info.plist` for
AppKit to read, so `crates/phoebus-app/src/icon.rs` sets the Dock icon at runtime instead —
the plate PNG, which is the best a process with no bundle can do, since
`setApplicationIconImage:` takes an image and not an icon asset.

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

`icon.json` restates the same two colours as its background `fill` and its one layer's
`fill`, because the `.icon` format keeps colour in the document and not in the artwork —
`Assets/mark.svg` is a stencil, and Icon Composer paints it. The rim light has no
counterpart there and does not need one: Tahoe lights the edge itself.

Neither SVG is a trace of the reference. Every number below was measured off it and the
shapes re-drawn from those measurements, which is why the geometry can be stated as exact
fractions at all.

Geometry, as fractions of the plate (or of the whole canvas, for the full-bleed variant):
ring outer diameter 86.2 % with a 13.5 % stroke; the play triangle equilateral with corner
radius 10 % of the ring's inner radius, inscribed in the inner circle — the three corner
arcs sit on the `r_inner − r_corner` circle about the ring centre, so each is exactly
tangent to the ring and all three corners kiss it. The reference measures the same tangency
(corner reach 309.2 vs inner radius 308.5 on its 1043 px canvas).

`Assets/mark.svg` is that mark after the 0.85 shrink the other two apply with a transform,
baked into the path data instead: outer radius 375.14, inner 257.63, 73.2 % of the canvas
across. It has no strokes at all — Icon Composer flattens a stroked shape to the whole
region its outline encloses, which turns the ring into a solid disc — so the ring is an
even-odd annulus of two circles.

The plate outline is traced from macOS 26 itself, not drawn from a formula: the mask the
system composites for its own apps (IconServices' 1024 px render of Music.app) was sampled
at sub-pixel precision along its alpha edge, the four corners folded together for noise,
and the corner rebuilt as eight cubics through the measured boundary with straight edges
between. A superellipse `|x/412|^5 + |y/412|^5 = 1` — the previous outline — agrees at the
45° point but carries visibly more material at the corner shoulders, which is exactly where
it read "rounder" than the neighbours in a Dock. Re-rasterised at 1024, the fitted path
disagrees with the system mask only on isolated antialiased edge pixels.
The `.icon` has no plate to get right; only the `.icns` and the window icon need this one.
