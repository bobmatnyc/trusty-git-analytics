<script lang="ts">
	import { SUBCOMMANDS, type Subcommand } from '$lib/site';

	const GROUPS = ['Pipeline', 'Reporting', 'External sync', 'Setup', 'Other'] as const;

	function inGroup(group: (typeof GROUPS)[number]): Subcommand[] {
		return SUBCOMMANDS.filter((s) => s.group === group);
	}
</script>

<svelte:head>
	<title>Usage — tga</title>
	<meta
		name="description"
		content="Every tga subcommand, grouped by what it does: the collect/classify/report pipeline, reporting, external sync, and setup."
	/>
</svelte:head>

<section class="border-b border-foundry-border">
	<div class="mx-auto max-w-content px-4 py-14 sm:px-6 sm:py-20">
		<p class="eyebrow">tga</p>
		<h1 class="mt-4 font-display text-4xl font-bold tracking-tight sm:text-5xl">Usage</h1>
		<p class="mt-6 max-w-2xl text-lg text-foundry-secondary">
			Every <code>tga</code> subcommand. Run <code>tga &lt;command&gt; --help</code> for the full flag
			reference on any one of them.
		</p>
	</div>
</section>

<section class="mx-auto max-w-content px-4 py-16 sm:px-6">
	<div class="prose-block">
		<h2 class="!mt-0">Global options</h2>
		<p>
			Every subcommand accepts <code>-c</code>/<code>--config</code> (path to
			<code>config.yaml</code>, default <code>./config.yaml</code>), <code>-d</code>/<code
				>--database</code
			>
			(path to the SQLite database, otherwise resolved from config), <code>-v</code> /
			<code>-vv</code> / <code>-vvv</code> for increasing verbosity, and
			<code>--log &lt;LEVEL&gt;</code>
			to set an exact log level.
		</p>

		{#each GROUPS as group (group)}
			<h2>{group}</h2>
			<div class="doc-table">
				<table>
					<thead>
						<tr>
							<th>Command</th>
							<th>What it does</th>
						</tr>
					</thead>
					<tbody>
						{#each inGroup(group) as sub (sub.name)}
							<tr>
								<td><code>{sub.name}</code></td>
								<td>{sub.description}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/each}
	</div>
</section>
