<script lang="ts">
	import { page } from '$app/state';
	import '../app.css';
	import SiteHeader from '$lib/components/SiteHeader.svelte';
	import SiteFooter from '$lib/components/SiteFooter.svelte';
	import { SITE_URL } from '$lib/site';

	let { children } = $props();

	/**
	 * Why: every route needs one canonical URL and a matching `og:url`, set
	 * once here rather than repeated per page.
	 * What: `SITE_URL` plus the current route's pathname, recomputed whenever
	 * `$page.url` changes so client-side navigation keeps it correct even
	 * though every route is prerendered.
	 * Test: `tests/build-smoke.test.ts`, "emits a canonical link and og:url
	 * pointing at tga.trustytools.dev".
	 */
	const canonicalUrl = $derived(`${SITE_URL}${page.url.pathname}`);
</script>

<svelte:head>
	<link rel="canonical" href={canonicalUrl} />
	<meta property="og:url" content={canonicalUrl} />
	<meta property="og:site_name" content="tga.trustytools.dev" />
</svelte:head>

<!-- Keyboard users land here first; the target must be focusable, hence tabindex. -->
<a
	href="#main"
	class="sr-only rounded border-[1.5px] border-foundry-primary bg-foundry-card px-4 py-2 focus:not-sr-only focus:absolute focus:left-4 focus:top-4 focus:z-50"
>
	Skip to content
</a>

<div class="flex min-h-screen flex-col">
	<SiteHeader />
	<main id="main" tabindex="-1" class="flex-1 focus:outline-none">
		{@render children()}
	</main>
	<SiteFooter />
</div>
