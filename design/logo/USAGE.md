# istmo brand guidelines

**Version:** 1.0 draft
**Palette:** C — Mono + Acid
**Mark:** B1 ring with signal leads

## The mark

istmo's mark is a ring with two signal leads. The ring is a node. The leads are the wires that reach it. The isthmus is the wire. Do not describe it as a resistor, a target, or a port in public copy — call it the mark.

## Files

| Purpose | File |
|---|---|
| Symbol master | `istmo-symbol.svg` |
| Symbol small-size cut (16-24 px) | `istmo-symbol-small.svg` |
| Wordmark master | `istmo-wordmark.svg` |
| Horizontal lockup | `istmo-lockup-horizontal.svg` |
| Stacked lockup | `istmo-lockup-stacked.svg` |
| Favicon pack | `variants/favicon-*.png`, `variants/favicon.ico`, `variants/favicon.svg` |
| App icon | `variants/istmo-symbol-app-icon.svg`, `variants/icon-192.png`, `variants/icon-512.png` |
| Apple touch | `variants/apple-touch-icon.png` |
| Web manifest | `variants/site.webmanifest` |
| HTML head snippet | `variants/head-snippet.html` |

## Colour

Palette C — Mono + Acid.

| Role | Light | Dark |
|---|---|---|
| Paper (background) | `#F5F5F2` | `#0A0A0A` |
| Ink (foreground) | `#0A0A0A` | `#F5F5F2` |
| Accent | `#BFFF00` | `#BFFF00` |
| Signal (success, ships) | `#0A8A3E` | `#0A8A3E` |
| Muted | `#6B6B6B` | `#6B6B6B` |
| Divider | `#E5E5E2` | `#1F1F1F` |

**Accent policy.** Acid lime `#BFFF00` is ownable. Reserve it for:
- The mark in brand headers (navbar logo, hero)
- One primary CTA per screen (first "Get started" button, submit of a form)
- Signal pills that need to stand out (new badge, release dot)

Do not use lime as body text, as section background, or as a wash. If it appears twice on the same viewport, remove one.

**Contrast.** Lime on paper `#F5F5F2` fails WCAG AA for small text. Use lime ONLY for the mark, interactive accents (buttons with ink text on lime, 48px minimum), or for 24px+ labels. For body copy use ink on paper; for muted secondary use `#6B6B6B` on paper (contrast 4.6:1, AA).

## Clear space

Clear space around any lockup = height of the symbol's ring (half the symbol height). No type, no image, no other element inside that zone.

## Minimum sizes

| Application | Minimum |
|---|---|
| Symbol (favicon, avatar) | 16 px — use `istmo-symbol-small.svg` |
| Symbol (anywhere else) | 24 px — use `istmo-symbol.svg` |
| Horizontal lockup | 120 px wide |
| Stacked lockup | 80 px wide |

Below minimums use the symbol only, never scale the lockup smaller.

## What not to do

- Do not rotate the mark.
- Do not add a stroke, drop shadow or gradient to the mark.
- Do not recolour the leads or ring with different hues from each other.
- Do not rebuild the wordmark in a different typeface. The letter shapes in `istmo-wordmark.svg` are the wordmark.
- Do not pair the mark with Rust's gear or crab glyph — do not imply istmo is a Rust project visual; the mark stands alone.
- Do not use the mark on backgrounds other than paper `#F5F5F2`, deep `#0A0A0A`, or acid `#BFFF00` (use ink symbol on acid).
- Do not use Unicode arrows, middle dots or em dashes next to the mark in text. Plain spacing, plain punctuation.

## Approved backgrounds

| Background | Mark |
|---|---|
| `#F5F5F2` (paper) | Ink `#0A0A0A` |
| `#0A0A0A` (deep) | Paper `#F5F5F2` |
| `#BFFF00` (acid) | Ink `#0A0A0A` |
| Photograph (clean, high contrast) | Paper mark on dark areas; use with 40% safety margin |

Do not place the mark on photograph zones with competing shapes (circuit boards, electronics, rings).

## Typography

Primary: **Geist** for body and headings.
Monospace: **Geist Mono** for code, labels, version strings.

The wordmark is the wordmark — do not use Geist typed out as a replacement. For wordmark usage in copy (`istmo lets you…`), use Geist regular lowercase.

## Rust-specific notes

- GitHub avatar: use `variants/istmo-symbol-app-icon.svg` (ink symbol on deep tile).
- Cargo.toml `description`: describe the project in one plain sentence. Do not include the mark or the wordmark.
- crates.io README logo: use `variants/istmo-lockup-horizontal-black-1600.png` at 400px width.
- Docs favicon: `variants/favicon.ico` + `variants/favicon.svg` + the head snippet.

## Trademark

No clearance search was run. Recommend a professional trademark search before registering the mark or publishing swag with it.

## Changelog

- 1.0 draft — Phase 7 kit, palette C, mark B1, wordmark constructed as paths.
