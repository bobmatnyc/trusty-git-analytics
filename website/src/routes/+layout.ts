/**
 * Why: this is a marketing and documentation site with no per-request state,
 * so every route should be HTML on a CDN rather than a serverless invocation.
 * What: prerender everything; `ssr` stays on and `csr` is left at its default
 * for the theme toggle to hydrate.
 */
export const prerender = true;
