# Graftline brand

A desktop Git client built around the commit graph. A graft joins a branch to a trunk; the symbol shows that join.
Tagline: **See the history / Shape it safely**.

| Token | Hex |
|---|---|
| Teal (primary) | `#14B8A6` |
| Mint | `#5EEAD4` |
| Deep teal (primary on light backgrounds) | `#0D9488` |
| Amber (the grafted branch) | `#F59E0B` |
| Ink (text) | `#10201D` |
| Night (dark background) | `#0B1211` |
| Canvas | `#F3F7F6` |

Typography: **Inter** (UI, brand), **JetBrains Mono** (code, hashes, diffs).

Graftline is a sibling of Rowveil: the same layout, components and third-party themes. The accent differs (teal in place of indigo), so the two apps are told apart at a glance.

Sources (vector masters) live in `assets/brand/`:

- `symbol.svg` — the trunk with a grafted branch;
- `app-icon.svg` — the app icon tile;
- `logo-horizontal.svg`, `logo-horizontal-dark.svg` — lockups with the tagline.

The favicon is `assets/brand/favicon.svg`. Raster icons (`assets/graftline.ico`, `assets/icons/*.png`), the installer bitmaps and the icons of the website are drawn by `assets/brand/make_icons.py` from the same geometry:

```
python assets/brand/make_icons.py
```
