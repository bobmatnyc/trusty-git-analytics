<script lang="ts">
	import { CRATES_IO_URL, FACTS, GITHUB_URL } from '$lib/site';
	import PageMeta from '$lib/components/PageMeta.svelte';
</script>

<PageMeta
	title="tga — developer productivity analytics from git history"
	description="tga walks git repositories into SQLite, classifies every commit through a tiered cascade, and reports per-author and per-week velocity, quality, and DORA metrics."
/>

<!-- HERO -->
<section class="border-b border-foundry-border">
	<div class="mx-auto max-w-content px-4 py-14 sm:px-6 sm:py-20">
		<p class="eyebrow">trusty-git-analytics</p>
		<h1
			class="mt-4 break-words font-display text-4xl font-bold leading-tight tracking-tight text-foundry-primary sm:text-5xl"
		>
			tga
		</h1>
		<p class="mt-6 max-w-2xl text-lg text-foundry-secondary">
			Turns git history into per-author and per-week reporting, with a classification cascade that
			names the work each commit did.
		</p>

		<div class="mt-8 flex flex-wrap gap-3">
			<a href="/install" class="btn btn-primary">Install</a>
			<a href="/docs/usage" class="btn btn-secondary">Usage</a>
			<a href={GITHUB_URL} rel="noreferrer noopener" class="btn btn-secondary">Source</a>
			<a href={CRATES_IO_URL} rel="noreferrer noopener" class="btn btn-secondary">crates.io</a>
		</div>

		<dl class="mt-12 flex flex-wrap gap-x-10 gap-y-4">
			{#each FACTS as fact (fact.label)}
				<div>
					<dt class="eyebrow">{fact.label}</dt>
					<dd class="mt-1 font-mono text-sm text-foundry-text">{fact.value}</dd>
				</div>
			{/each}
		</dl>
	</div>
</section>

<!-- WHAT IT DOES -->
<section class="mx-auto max-w-content px-4 py-16 sm:px-6">
	<div class="prose-block">
		<h2>What it does</h2>
		<p>
			<code>tga</code> walks one or more local git repositories, collects every commit into a SQLite
			database, classifies each commit into a work category (feature, bugfix, refactor, and so on)
			through a multi-tier classification cascade, then aggregates the results into per-author,
			per-week, DORA, velocity, and quality reports. It is a Rust port of
			<a
				href="https://github.com/bobmatnyc/gitflow-analytics"
				rel="noreferrer noopener"
				class="text-foundry-primary underline underline-offset-2">gitflow-analytics</a
			>, aiming for the same YAML config schema and the same SQLite schema.
		</p>
	</div>
</section>

<!-- THREE-STAGE PIPELINE -->
<section class="border-y border-foundry-border bg-foundry-raised">
	<div class="mx-auto max-w-content px-4 py-16 sm:px-6">
		<div class="prose-block">
			<h2>Three stages, one command</h2>
			<p>
				<code>tga analyze</code> runs the whole pipeline. Each stage is also its own subcommand, because
				on a large history you will want to re-run one without paying for the others.
			</p>
			<ul>
				<li>
					<code>tga collect</code> — walk each configured repository, extract commit metadata and diff
					stats, resolve author identities, and write it all to SQLite. Optionally pull pull-request and
					issue metadata from GitHub, JIRA, Linear, or Azure DevOps alongside it.
				</li>
				<li>
					<code>tga classify</code> — run every unclassified commit through the cascade and write the
					verdict back.
				</li>
				<li>
					<code>tga report</code> — aggregate per author, per week, and per DORA metric, then write CSV,
					JSON, and Markdown into the output directory.
				</li>
			</ul>
			<p>
				The database is a local SQLite file, so every number in a report is one you can go and check
				with a query.
			</p>
		</div>
	</div>
</section>

<!-- CLASSIFICATION CASCADE -->
<section class="mx-auto max-w-content px-4 py-16 sm:px-6">
	<div class="prose-block">
		<h2>The classification cascade</h2>
		<p>
			A single heuristic gets commit classification wrong often enough to be useless, so
			<code>tga</code> tries tiers in order and takes the first confident answer: a manual override you
			pinned, the issue type from a linked ticket, a project-key mapping, a multi-pattern scan for conventional-commit
			prefixes, regex patterns, a weighted sum over several independent signals, and heuristics for merges
			and reverts. An LLM tier sits at the end for the commits the rules could not place.
		</p>
	</div>
</section>

<!-- TRUSTY-AUDIT TEASER -->
<section class="border-y border-foundry-border bg-foundry-raised">
	<div class="mx-auto max-w-content px-4 py-16 sm:px-6">
		<div class="card flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
			<div>
				<h2 class="font-display text-xl font-semibold">Also in this repository: trusty-audit</h2>
				<p class="mt-1 max-w-2xl text-sm text-foundry-secondary">
					The auditor client that installs its own pinned copies of tga, trusty-search,
					trusty-analyze, and trusty-review, then drives a due-diligence sweep and returns a signed
					report.
				</p>
			</div>
			<a href="/trusty-audit" class="btn btn-primary shrink-0">trusty-audit →</a>
		</div>
	</div>
</section>
