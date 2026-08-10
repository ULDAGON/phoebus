# Bundled assets

Everything here is compiled into the binary with `include_bytes!` — there is no runtime
asset path and nothing to install next to the executable.

## `NotoSansJP-Regular.otf` — 4 533 028 bytes

The Japanese-subset, static (non-variable) build of Noto Sans CJK / Source Han Sans,
downloaded verbatim from the upstream Noto repository:

    https://raw.githubusercontent.com/notofonts/noto-cjk/main/Sans/SubsetOTF/JP/NotoSansJP-Regular.otf

    sha256  dff723ba59d57d136764a04b9b2d03205544f7cd785a711442d6d2d085ac5073

Licence: **SIL Open Font License, Version 1.1** — full text in `LICENSE-NotoSansJP.txt`
(<https://raw.githubusercontent.com/notofonts/noto-cjk/main/Sans/LICENSE>). Copyright
2014–2021 Adobe (<http://www.adobe.com/>). The OFL permits bundling and redistribution
inside a binary; the font is shipped unmodified and keeps its reserved name.

Why this file: it is the smallest single-file option that covers Japanese kana + kanji
(the pan-CJK `NotoSansCJKjp-Regular.otf` is ≈16 MB and the variable
`NotoSansJP[wght].ttf` ≈9.6 MB), and being static there is no variable-font default
instance for the rasteriser to get wrong.

`theme::install_fonts` registers it as the **last** fallback of the `Monospace` and
`Proportional` families (UI-SPEC v1.2 §CJK), so Latin keeps the bundled mono face and only
the codepoints that face lacks come from here.

## `LICENSE-NotoSansJP.txt`

The SIL OFL 1.1 text shipped with the font above. Not compiled in; it is here so the
licence travels with the file it covers.

## `Phosphor.ttf` — 488 636 bytes

Phosphor Icons, **Regular** weight — every icon in the app, downloaded verbatim from the
upstream icon-font repository:

    https://raw.githubusercontent.com/phosphor-icons/web/master/src/regular/Phosphor.ttf

    sha256  06b91e022b7ee899a63efced879392a74f0bacbda54e4467e9f663220d173a10

Licence: **MIT** — full text in `LICENSE-Phosphor.txt`
(<https://raw.githubusercontent.com/phosphor-icons/web/master/LICENSE>). Copyright
2020–2021 Phosphor Icons. The MIT licence permits bundling and redistribution inside a
binary; the font is shipped unmodified.

Why a bundled font and not the `egui-phosphor` crate: the newest release of that crate
(0.13.0, 2026-07-22) declares `egui ^0.35`, and this workspace is on egui 0.36.1. There is
no version of it that can be linked against the egui we build with — its
`add_to_fonts(&mut FontDefinitions, Variant)` would take *its* egui's types, not ours — so
the font is registered directly instead and the codepoints are named in `theme.rs`.

Why the whole face and not a subset: a subset would be an artefact of this repository
rather than a file anyone can re-download and check against the sha256 above, and 0.5 MB
next to the 4.5 MB CJK fallback is not worth that. The codepoints are the ones in
`src/regular/style.css` of the same upstream directory.

All 1 513 glyphs sit in the Private Use Area (U+E000–U+EE83), so the face cannot answer for
any character real text contains. `theme::install_fonts` registers it as a fallback of the
`Monospace` and `Proportional` families, *between* the bundled mono face and
`NotoSansJP-Regular.otf` — being in the same family as the text is what lets one galley
carry a 14 px icon and an 11 px label (`widgets::icon_text`).

## `LICENSE-Phosphor.txt`

The MIT licence text shipped with the font above. Not compiled in; it is here so the
licence travels with the file it covers.

## `Phosphor-Fill.ttf` — 449 252 bytes

Phosphor Icons, **Fill** weight — the one weight the outline face cannot supply, used for a
hearted heart (UI-SPEC v1.3 §Favorites). Downloaded verbatim from the same upstream
icon-font repository:

    https://raw.githubusercontent.com/phosphor-icons/web/master/src/fill/Phosphor-Fill.ttf

    sha256  a53f5d2630cab5e3b7536ecb9d69d71519a2190298c22b1f8d770dd37bc2940a

Licence: **MIT** — full text in `LICENSE-Phosphor-Fill.txt`
(<https://raw.githubusercontent.com/phosphor-icons/web/master/LICENSE>, the same licence
that covers `Phosphor.ttf`). Copyright 2020–2021 Phosphor Icons. The font is shipped
unmodified.

Why a SEPARATE egui font family (`phosphor-fill`) rather than another fallback of
`Monospace` / `Proportional`: the two weights map the **same** codepoints. `heart` is
U+E2A8 in `src/regular/style.css` *and* in `src/fill/style.css`, and every other icon
likewise — the variants are the same icon list drawn twice. A fallback chain resolves a
codepoint at the first face that carries it, so a fill face appended to the text families
could never be reached, and one inserted ahead of `Phosphor.ttf` would turn every icon in
the app solid. `theme::install_fonts` therefore registers it as the sole member of
`FontFamily::Name("phosphor-fill")`, reachable only through `theme::font_icon_fill`.

## `LICENSE-Phosphor-Fill.txt`

The MIT licence text shipped with the font above — byte-identical to
`LICENSE-Phosphor.txt`, kept as its own file so the licence travels with the file it
covers.
