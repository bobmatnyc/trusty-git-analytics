# tga website

The public marketing/docs site for `tga` and `trusty-audit`, meant to deploy
to Vercel from this subdirectory. SvelteKit + Svelte 5 runes + Tailwind,
themed with Foundry v2 — the same stack and versions as
[trusty-tools/website](https://github.com/bobmatnyc/trusty-tools/tree/main/website),
trimmed to a single-product site: no multi-tool catalogue, no markdown docs
reader, no changelog corpus.

Cargo does not see this directory — the workspace's `members` list in the
root `Cargo.toml` is `[".", "crates/trusty-audit"]` — so nothing here affects
a Rust build.

`tga` previously lived in the `bobmatnyc/trusty-tools` monorepo; this
repository is its own, split out (see the root `Cargo.toml` header comment).

## Local development

```bash
cd website
pnpm install
pnpm dev        # http://localhost:5173
```

| Command          | What it does                             |
| ---------------- | ---------------------------------------- |
| `pnpm dev`       | Dev server with HMR                      |
| `pnpm build`     | Production build into `.vercel/output/`  |
| `pnpm preview`   | Serve the production build locally       |
| `pnpm check`     | `svelte-check` typecheck                 |
| `pnpm lint`      | `prettier --check` + `eslint`            |
| `pnpm format`    | Rewrite with Prettier                    |
| `pnpm test`      | Vitest `unit` + `smoke` projects         |
| `pnpm test:unit` | Vitest `unit` project only (fast, jsdom) |

pnpm `9.15.9`, pinned in `packageManager`, matching trusty-tools/website's pin.

`pnpm test` shells out to a real `vite build` twice (once per file in
`tests/`) and drives a real Chromium for the mobile-overflow check, so it is
slower than `pnpm test:unit`. CI runs it in
[`.github/workflows/website.yml`](../.github/workflows/website.yml), scoped
to changes under `website/**`.

## Pages

| Route           | What it covers                                              |
| --------------- | ----------------------------------------------------------- |
| `/`             | What `tga` does, the three-stage pipeline, install teaser   |
| `/install`      | `cargo install tga --locked`, GitHub Releases, first run    |
| `/docs/usage`   | Every `tga` subcommand, grouped                             |
| `/trusty-audit` | The `trusty-audit` crate and `tga audit`'s stages and flags |

Every factual claim (a command, a flag, a stage count) is checked against
`src/main.rs`, `src/audit/`, or `crates/trusty-audit/` in this repository —
not against a README, several of which in this repository still describe the
pre-split `bobmatnyc/trusty-tools` monorepo. `src/lib/site.test.ts` re-derives
the MSRV, license, and subcommand list from the repository and fails if
`src/lib/site.ts` drifts from it.

## Vercel setup (Bob's to configure; nothing deploys from this repository)

Owner ruling: Vercel project `trusty-git-analytics`, domain
`tga.trustytools.dev`.

| Setting                                              | Value                                                                                                                                                                                           |
| ---------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Project name                                         | `trusty-git-analytics`                                                                                                                                                                          |
| Framework preset                                     | SvelteKit (auto-detected)                                                                                                                                                                       |
| Root Directory                                       | `website`                                                                                                                                                                                       |
| Build Command                                        | default (`vite build`, auto-detected)                                                                                                                                                           |
| Output Directory                                     | default (`.vercel/output`, auto-detected)                                                                                                                                                       |
| Environment variables                                | none required                                                                                                                                                                                   |
| Domain                                               | `tga.trustytools.dev` — the zone is on Vercel DNS under the account that owns `trustytools.dev`, so adding the domain to the project creates the record; nothing to configure in DNS separately |
| "Include source files outside of the Root Directory" | not needed — this package reads nothing outside `website/` at build time, unlike trusty-tools/website's docs reader                                                                             |

`src/lib/site.ts`'s `SITE_URL` is `https://tga.trustytools.dev`, and
`+layout.svelte` emits `<link rel="canonical">` and `og:url` from it on every
page.

### Ignored Build Step

```
git diff --quiet HEAD^ HEAD -- "$(git rev-parse --show-toplevel)/website"
```

The pathspec is absolute, resolved from the repository toplevel rather than
the process's working directory, so the command gives the same answer
regardless of which directory it runs from. Vercel skips the build when the
command exits 0. Verified against this repository's own history: a commit
touching `website/` exits 1 (build runs); a commit that does not exits 0
(build skipped).

This workflow adds no Vercel deployment configuration beyond what the build
itself needs (no `vercel.json`, no deploy step in CI) — deploying is a
decision for whoever creates the Vercel project.

## Theme

`src/app.css` carries the Foundry v2 token layer, hand-transcribed from the
same palette trusty-tools/website uses (`docs/design/UI/design-system/tokens.css`
in that repository, checked at commit `603e6039` — this repository does not
have a copy of that file to diff against automatically, unlike
trusty-tools/website's `tokens.test.ts`).

- Light tokens: `:root` · dark tokens: `.dark`
- `src/lib/theme/index.ts` is the only writer of the `.dark` class on
  `<html>`, matching `darkMode: 'class'` in `tailwind.config.js`.
- `src/app.html` inlines a pre-paint snippet that sets the same class from
  the same `localStorage` key, so a dark-mode reader never sees a light-theme
  flash.

## Fonts

IBM Plex Sans, Chakra Petch, and IBM Plex Mono are self-hosted from
`static/fonts/` with their OFL licences, copied from trusty-tools/website at
`603e6039`. No CDN reference anywhere.
