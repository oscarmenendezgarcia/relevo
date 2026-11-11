# Relevo's mark

The R that passes the baton: its leg ends in an arrow that carries on, as a note goes from your agent to a teammate's.

| File | Use |
|---|---|
| `relevo-mark.svg` | The mark on its tile, 48 px and up. |
| `relevo-mark-small.svg` | The same with heavier strokes, for 16 to 32 px (favicons). |
| `relevo-glyph.svg` | The R alone in one colour (`currentColor`), on any background. |
| `favicon.ico`, `favicon-16.png`, `favicon-32.png` | Browser tab (from `relevo-mark-small.svg`). |
| `apple-touch-icon.png` (180), `relevo-mark-512.png` | Home-screen icon and large uses. |
| `relevo-logo-on-dark.png`, `relevo-logo-on-light.png` | Mark and name, for dark and light backgrounds (Inter ExtraBold, 2×). |

**Colours.** Tile gradient `#7c9cff` → `#b28cff` (top left to bottom right); R `#0b0c0f`. On light backgrounds the name is `#15161a`, on dark `#e7e9ee`.

**Rules.** Keep the arrow along the leg's diagonal; don't recolour the R inside the tile; leave at least a quarter of the mark's width clear around it. Below 48 px use `relevo-mark-small.svg`.

The PNGs are rendered from the SVGs; render them again when the SVGs change.
