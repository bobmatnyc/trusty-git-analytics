<script lang="ts">
	/**
	 * Why: issue #127 — every page already had a unique `<title>` and
	 * `<meta name="description">`, but neither fed `og:title`/`og:description`,
	 * so a shared link left every route's social-preview card blank. Routing
	 * both the native tags and the Open Graph tags through one prop pair keeps
	 * a single source of truth per page instead of a second copy of the same
	 * string in each route's `<svelte:head>`.
	 * What: renders `<title>` + `meta[name=description]` plus
	 * `og:title`/`og:description` (mirroring the same two props),
	 * `og:type=website`, and `twitter:card=summary`. `og:url` and
	 * `og:site_name` stay in `+layout.svelte`, which already owns them
	 * globally; no `og:image` — there is no image asset to point at.
	 * Test: `tests/build-smoke.test.ts`, "emits og:title and og:description
	 * matching each page's title and description".
	 */
	interface Props {
		/** Exact text of this page's `<title>` and `og:title`. */
		title: string;
		/** Exact text of this page's `meta[name=description]` and `og:description`. */
		description: string;
	}

	let { title, description }: Props = $props();
</script>

<svelte:head>
	<title>{title}</title>
	<meta name="description" content={description} />
	<meta property="og:title" content={title} />
	<meta property="og:description" content={description} />
	<meta property="og:type" content="website" />
	<meta name="twitter:card" content="summary" />
</svelte:head>
