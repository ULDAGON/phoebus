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
icon/Phoebus.icon/Assets/*-grain.png
                              generated — the grain layers of the macOS 26 icon
icon/Phoebus.icns             generated — the macOS 11…15 bundle icon, 16…1024 with @2x pairs
icon/hicolor/<N>x<N>/apps/    generated — the Linux icon theme set, 32/48/64/128/256/512
icon/phoebus-256.png          generated — compiled into the binary (window + Dock icon)
linux/phoebus.desktop         the launcher entry template
```

The generated files come from `scripts/make-icons.sh`, which needs `rsvg-convert`
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
at 1024, with the previous mark: with only an `.icns`, its ring spanned 408 px. With
`Assets.car`, 604 px — which is the size it is drawn at. Both put the plate at 824 px of
1024, the macOS grid. The current mark has not been measured that way.

So the `.icon` carries the mark on the background gradient and no plate at all, and lets
Tahoe do the compositing. `scripts/bundle-macos.sh` compiles it with `actool`, which ships
inside Xcode; a machine with only the Command Line Tools still gets a bundle, just one that
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

A blue-black field and a neon yellow mark, both slightly graded and both grained:

- **Field:** a linear gradient from `#12161C` to `#222D3D` along a line 15° off vertical
  through the centre, so it lightens towards the top right. Its ends are half a body (the
  plate, or the whole canvas) from the centre, so the corners beyond them are flat.
- **Mark:** a radial gradient from `#FFF46A` at the centre to `#FFF829` at 1.15 × the
  ring's outer radius — nearly flat.
- **Grain:** grey fractal noise (`feTurbulence`) blended with `overlay` at 60 % opacity,
  one layer on the field (base frequency 0.35, seed 7) and one masked to the mark (0.71,
  seed 13). The frequency is per user unit and the same in both SVGs: the grain is a
  texture, not geometry, so it does not scale with the body.
- **Rim light**, plate only: white at 23 % opacity along the top edge, fading to nothing by
  y = 600, a 4 px stroke clipped to the plate so only its inner half shows. The full-bleed
  square has no edge for a light to catch.

The yellow is deliberately *not* the app's `#FFFB00` accent — the icon is the brand mark,
the accent is a UI token, and they are allowed to differ.

Geometry, as fractions of the body (the 824 plate, or the whole 1024 canvas for the
full-bleed variant): ring outer diameter 68.2 % with a 12 % stroke; the play triangle
equilateral, pointing right, its centroid on the ring centre, its circumradius 0.89 of the
ring's inner radius and its corners rounded to 5 % of the circumradius. On the plate that
is a ring of radius 231.54 at the stroke centre and width 98.88, and a triangle of
circumradius 162.07; the full-bleed values are those scaled by 1024 / 824.

`Assets/mark.svg` is the full-bleed mark, because Tahoe draws the `.icon` canvas whole at
the size of its plate: outer radius 349.18, inner 226.30, 68.2 % of the canvas across. It
has no strokes at all — Icon Composer flattens a stroked shape to the whole region its
outline encloses, which turns the ring into a solid disc — so the ring is an even-odd
annulus of two circles.

`icon.json` restates the colours, because the `.icon` format keeps colour in the document
and not in the artwork — `mark.svg` is a stencil, and Icon Composer paints it:

- The background `fill` is the field gradient, vertical and lightest at the top, so its
  colours are listed light first. actool ignores an `orientation` key on this fill: with
  one, and with its y values swapped, the compiled icon came out the same, first colour
  on top. The 15° tilt of the SVGs is lost; the tilt is small and the light stays on top.
- The mark layer's `fill` is solid `#FFF73B`, the radial gradient's colour at the ring's
  stroke centre: `icon.json` has no radial fill.
- The grain is the two PNG layers `scripts/make-icons.sh` renders from the full-bleed SVG.
  `mark-grain.png` sits above the mark in its group with `blend-mode` `overlay` and
  `opacity` 0.6. `field-grain.png` has a group of its own below the mark, and the group
  carries the `overlay` and the 0.6 so that they apply to the background; its shadow,
  specular highlight and translucency are off.
- The rim light has no counterpart and does not need one: Tahoe lights the edge itself.

Apple publishes no schema for `icon.json`. Every key above is one that Icon Composer writes
into the `.icon` documents of public repositories, and Xcode 26.6's actool compiles this
document without a warning. Rendered through `NSWorkspace` from the compiled `Assets.car`,
the field runs from `#222D3D` at the top to `#12161C` at the bottom, with the grain on it.

The plate outline is traced from macOS 26 itself, not drawn from a formula: the mask the
system composites for its own apps (IconServices' 1024 px render of Music.app) was sampled
at sub-pixel precision along its alpha edge, the four corners folded together for noise,
and the corner rebuilt as eight cubics through the measured boundary with straight edges
between. A superellipse `|x/412|^5 + |y/412|^5 = 1` — the previous outline — agrees at the
45° point but carries visibly more material at the corner shoulders, which is exactly where
it read "rounder" than the neighbours in a Dock. Re-rasterised at 1024, the fitted path
disagrees with the system mask only on isolated antialiased edge pixels.
The `.icon` has no plate to get right; only the `.icns` and the window icon need this one.
