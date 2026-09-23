import adapter from '@sveltejs/adapter-vercel';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/**
 * Why: this is a single-product marketing/docs site for `tga` and
 * `trusty-audit`, deployed to Vercel from the `website/` subdirectory of this
 * Rust workspace — the same shape trusty-tools/website uses (adapter-vercel
 * from a subdirectory), trimmed to one product's worth of routes.
 * What: SvelteKit config. `adapter-vercel` emits `.vercel/output/` in the
 * Build Output API v3 layout; every route here is prerendered
 * (`src/routes/+layout.ts`), so the adapter writes static HTML and
 * provisions no serverless function. The Node runtime is pinned so a Vercel
 * default-runtime bump cannot silently change the build.
 * Test: `pnpm build` in `tests/build-smoke.test.ts` asserts the prerendered
 * `index.html` lands in `.vercel/output/static/`.
 *
 * Unlike trusty-tools/website, this package reads nothing outside its own
 * directory at build time — no `$repo` alias, no "Include source files
 * outside of the Root Directory" Vercel setting required.
 */
export default {
	preprocess: vitePreprocess(),
	kit: {
		adapter: adapter({ runtime: 'nodejs20.x' })
	}
};
