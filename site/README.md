# Importin's website

One page for Importin, the read-only MCP server for Apple Mail in this repository: what it reads, what it can never
do, how to install it and add it to an MCP host. Plain static files, served by GitHub Pages from `site/` at
`https://glia-org.github.io/importin/` once the repository is public; see [Before publishing](#before-publishing).

## Files

| path | what |
|---|---|
| `index.html` | the page: lead, the pore, how it works, the six tools with a drawn example, S/MIME, read-only, install, measured times, the name |
| `assets/site.css` | the look, and the pore's keyframes |
| `assets/fonts/literata-latin.woff2`, `assets/fonts/OFL.txt` | Literata, its Latin characters (the file Lemna's page serves), with its licence |
| `assets/favicon.svg` | the mark (M1) in its hinted 16 px form, in the accent: `#97a2ff` in a dark tab bar, `#4a3d9a` in a light one |
| `assets/apple-touch-icon.png` | the mark at 180 × 180 px on the page's ground, rendered with headless Chrome (`--screenshot --window-size=180,180`) from the 32-unit form used in the header |
| `.nojekyll` | tells GitHub Pages to serve the files as they are |
| `../checks/site.py` | the checks below; run before every commit that touches `site/` |
| `../.github/workflows/pages.yml` | publishes `site/` on every push to `main` that touches it, once Pages is switched on |

There is no script. The pore is inline SVG animated by CSS; its pause control is a checkbox read by CSS (`:has`).

## Preview

`open site/index.html`, or from a Glia session `glia show site/index.html`. No server is needed.

## Rules

The family's rules (glia's site README, rules 1–8), as they apply to this page:

1. **Plain static files.** No build step, no framework, no package manager.
2. **No third-party requests.** The font is self-hosted with its licence; no CDN, analytics, cookies or embeds. External
   URLs appear only as `<a href>` links, plus the canonical URL of this page. `checks/site.py` enforces it.
3. **Every claim true of the build as released** (v0.1.0). `README.md` is the source of truth; where the page goes
   into more detail on the read-only test, it follows `DESIGN.md` §2 and `tests/readonly.rs`. What is not built or not published is not offered (`cargo install importin` waits for the
   crates.io publish); gaps are named (signatures are not verified; the openssl path for `authEnveloped-data` is
   untested). Numbers say where they were measured. No superlatives, no exclamation marks.
4. **Plain and precise wording.** Descriptive headlines; per section a headline and at most four points; no slogans; no
   text on how to use the page.
5. **Nothing hidden behind a switch.** All three host snippets are visible at once; depth lives in the README and
   `DESIGN.md`.
6. **No real mail.** No message, address, mailbox name or count from a real store. Examples are synthetic: Erika
   Muster, Max Mustermann, `example.org`, dates in 2026.
7. **Complete without JavaScript; reduced motion honoured.** The page has no script. Under `prefers-reduced-motion`
   the pore stands still, each letter at a different point of its way in, and the pause control is hidden; for
   everyone else the control pauses it. Phones get one column and 16 px gutters.
8. **Importin's own look**: the picks below.

## Decisions

`A V2 P1 C2 S1 H1 M1 R2`, chosen by the maintainer on 2026-11-06 from `docs/design-panel-website-2026-11-06.html`,
which shows each option, its mockup and its trade-offs.

| | picked |
|---|---|
| A | the pore is the hero: envelopes with address tags carried in through a nuclear pore by importins, nothing travelling out |
| V2 | Literata for prose and headlines; code and transcripts in Menlo or the system mono, none downloaded |
| P1 | the pore in motion as the one piece; inline SVG and CSS keyframes, a still under reduced motion, a pause control |
| C2 | dark only, the panel's night palette: ground `#13131b`, surface `#1c1b26`, text `#e9e7f2`, copy `#b2afc4`, muted `#8a879c`, accent `#97a2ff`, membrane `#6c6794`, fills `#1f1a26` and `#191a2c`, hairline `#2d2c3a` |
| S1 | the full set of sections: lead, the piece, how it works, the six tools, S/MIME, read-only (and that read-only is not private), install for opencode, Claude Code and Claude Desktop, measured times, the name |
| H1 | `site/` in glia-org/importin, published by `.github/workflows/pages.yml` (Lemna's, with `actions/checkout@v5`) |
| M1 | the pore ring, eight short bars around a dot, one colour; never rotated or animated |
| R2 | pictures of the real thing are drawn stand-ins, labelled as such: one drawn opencode transcript, captioned "Drawn example, synthetic data" |

## Measured times

The figures in the "Measured times" section are placeholders of the form `⟨measure:start⟩` until they are filled from
the README's measurements: `setup` (the sentence that says where and on what they were measured), `start`,
`search-subject`, `search-participant`, `search-all`, `get-email`, `get-email-smime`, `get-thread`, `search-body`,
`list-mailboxes`. `python3 checks/site.py` fails while any is left; `--allow-placeholders` lets it pass meanwhile.

## Checks

`python3 checks/site.py` (standard library only) fails on:

- any external request: an external `src=`, a stylesheet or other `<link>` load from another origin, any `@import`,
  any `url(http…)` in CSS, inline styles or SVG files;
- any `http` URL in the HTML that is not a plain `<a href>` link (the canonical URL under
  `https://glia-org.github.io/importin/` excepted);
- leftover `⟨measure:…⟩` placeholders, unless `--allow-placeholders` is given;
- the words `blazing`, `powerful`, `seamless`, and `!` at a sentence end;
- a missing local file or `#fragment`, an id used twice, an element not closed in order.

It lists the external links and the size of every file the page loads.

## Before publishing

- [ ] glia-org/importin is public (GitHub Pages on the free plan needs it, and so do the release and source links).
- [ ] Pages is enabled for `main` with GitHub Actions as its source (Settings › Pages › Source: GitHub Actions).
- [ ] The `⟨measure:…⟩` placeholders are filled from the README, and the row labels of "Measured times" match what was
      measured.
- [ ] `python3 checks/site.py` passes, without `--allow-placeholders`.
- [ ] Every sentence still matches `README.md` (the Claude Desktop snippet and the line that read-only is not private
      included).
- [ ] Checked in Safari, Chrome and Firefox, on a phone, with JavaScript off and with reduced motion.

## Licences

The site is licensed under the Apache License 2.0, like Importin (`../LICENSE`).

Literata © The Literata Project Authors, under the SIL Open Font License 1.1 (`assets/fonts/OFL.txt`); this page serves
its Latin characters.
