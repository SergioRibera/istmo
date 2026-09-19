# Istmo docs

Astro Starlight documentation site for **istmo** — the Rust-first mobile
framework.

## Local development

```bash
cd docs
pnpm install     # or npm / yarn / bun
pnpm dev
```

The dev server serves the site at <http://localhost:4321>.

## Structure

- `src/content/docs/` — the site content. English is the source of truth
  and lives at the collection root. Translated copies live under `es/`.
- `astro.config.mjs` — sidebar, i18n, and Starlight options.
- `src/styles/theme.css` — visual overrides on top of Starlight
  defaults.

## Adding a new page

1. Create the English file: `src/content/docs/<section>/<slug>.md`.
2. Mirror the file at `src/content/docs/es/<section>/<slug>.md` (until
   translations are automated). If a translation is missing Starlight
   automatically falls back to English.
3. Frontmatter must include `title` and (optionally) `description`.

## Building

```bash
pnpm build     # generates ./dist
pnpm preview   # serves ./dist locally
```

The generated site is a fully static bundle — deploy to any static host
(Cloudflare Pages, Netlify, Vercel, GitHub Pages).
