# Skadi Design Language — "Cold Nordic"

Token-first dark theme. Slate surfaces, an ice/teal/violet
**aurora** accent set, and a **gold** "owned / cutoff" accent. This package is
the *portable design language* extracted from `crates/skadi-web/style.css` — the
tokens, type, and base layer. It does **not** contain components: the Skadi UI is
a Leptos→wasm app, so its components don't exist as separable JS/React units that
could be bundled here. This is the look, not the parts.

## Files
| File | What it is |
|---|---|
| `styles.css` | Entry — import this. Pulls in fonts + tokens, then the base layer. |
| `tokens/tokens.css` | The `:root` custom properties (single source of truth). |
| `tokens/tokens.json` | Same tokens, structured, for tooling / theme generators. |
| `fonts/fonts.css` | IBM Plex Sans + Mono via Google Fonts. |

## How to style with it
Everything is a CSS custom property — style with `var(--*)`, never hard-coded hex.

```html
<link rel="stylesheet" href="styles.css" />

<div style="background: var(--panel); border: 1px solid var(--border);
            border-radius: var(--r-card); color: var(--fg);">
  <span class="u-label">Status</span>
  <code class="mono tnum">1.4 GB</code>
</div>
```

## Token families
- **Surfaces** (darkest→lightest): `--inset` · `--bg` · `--panel-2` · `--panel` · `--sidebar` · `--control`
- **Borders**: `--border-fainter` · `--border-soft` · `--border` · `--border-control`
- **Text** (brightest→faintest): `--fg-bright` · `--fg` · `--fg-2` · `--muted` · `--faint` · `--fainter`
- **Accents**: `--ice` (primary, aliased as `--accent`) · `--teal` · `--violet` · `--gold` · `--brand-stroke`
- **Semantic**: `--ok` (green) · `--warn` (gold) · `--bad` (red)
- **Tinted fills**: `--ice-tint` (selected) · `--ice-tint-soft` (hover)
- **Gradients**: `--aurora` (teal→ice→violet brand sweep) · `--progress` (teal→ice)
- **Radii**: `--r-tile` 8px · `--r-card` 10px · `--r-pill` 20px

## Type idiom
Two families, with roles:
- `--sans` (IBM Plex Sans) — all UI text. Weights 400/500/600/700.
- `--mono` (IBM Plex Mono) — **numbers, paths, env vars, release titles,
  timestamps, technical labels.** Weights 400/500/600. Apply via `.mono`; add
  `.tnum` for tabular numerals; use `.u-label` for the small uppercase caption.

Reach for mono whenever you render a value a user could copy or compare digit-by-digit.
