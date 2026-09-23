<script lang="ts">
	import CommandBlock from '$lib/components/CommandBlock.svelte';
	import { GITHUB_URL } from '$lib/site';

	const SOURCE_URL = `${GITHUB_URL}/tree/main/crates/trusty-audit`;
	const AUDIT_SOURCE_URL = `${GITHUB_URL}/tree/main/src/audit`;

	const facts = [
		{ label: 'Crate', value: 'trusty-audit' },
		{ label: 'Runs on', value: 'macOS, Apple Silicon' },
		{ label: 'tga audit stages', value: '10' },
		{ label: 'tga audit flags', value: '7, all optional' }
	];

	const stages = [
		'collect',
		'correlate',
		'classify',
		'jira sync',
		'linear sync',
		'deployments',
		'incidents',
		'dora',
		'pr-metrics',
		'report'
	];

	const flags = [
		{
			flag: '--org <ORG>',
			note: 'GitHub organisation or Bitbucket workspace, used for the report title.'
		},
		{
			flag: '--title <TITLE>',
			note: 'Report title. Defaults to "<org> — Technical Due Diligence".'
		},
		{ flag: '--analyst <NAME>', note: 'Name of the analyst producing the report.' },
		{ flag: '--client <NAME>', note: 'Name of the client the report is produced for.' },
		{ flag: '-o, --output <DIR>', note: 'Output directory. Defaults to ./audit-output.' },
		{ flag: '--weeks <N>', note: 'Limit collection to the last N ISO weeks.' },
		{
			flag: '--no-render',
			note: 'Write the manifest and stop, leaving the report to a later render.'
		}
	];
</script>

<svelte:head>
	<title>trusty-audit — acquisition due diligence from git history</title>
	<meta
		name="description"
		content="trusty-audit is the auditor client: it installs its own pinned copies of tga, trusty-search, trusty-analyze, and trusty-review, then runs tga audit and returns a signed due-diligence report."
	/>
</svelte:head>

<section class="border-b border-foundry-border">
	<div class="mx-auto max-w-content px-4 py-14 sm:px-6 sm:py-20">
		<p class="eyebrow">Part of this repository · Acquisition due diligence</p>
		<h1
			class="mt-4 font-display text-4xl font-bold tracking-tight text-foundry-primary sm:text-5xl"
		>
			trusty-audit
		</h1>
		<p class="mt-6 max-w-2xl text-lg text-foundry-secondary">
			The auditor client: point it at a set of repositories, and it installs its own pinned tooling,
			drives a <code>tga audit</code> sweep, and returns one report describing what it found — and what
			it could not measure.
		</p>

		<div class="mt-8 flex flex-wrap gap-3">
			<a href="#install" class="btn btn-primary">Install</a>
			<a href={SOURCE_URL} rel="noreferrer noopener" class="btn btn-secondary">Source</a>
		</div>

		<dl class="mt-12 flex flex-wrap gap-x-10 gap-y-4">
			{#each facts as fact (fact.label)}
				<div>
					<dt class="eyebrow">{fact.label}</dt>
					<dd class="mt-1 font-mono text-sm text-foundry-text">{fact.value}</dd>
				</div>
			{/each}
		</dl>
	</div>
</section>

<section class="mx-auto max-w-content px-4 py-16 sm:px-6">
	<div class="prose-block">
		<h2 class="!mt-0">What it is</h2>
		<p>
			<code>trusty-audit</code> is a client that a recipient runs against their own codebases. It
			installs its own pinned copies of four tools — <code>tga</code>, <code>trusty-search</code>,
			<code>trusty-analyze</code>, and <code>trusty-review</code> — then registers the repositories and
			ticketing boards under audit, drives the sweep, and packages the result into a signed return package.
		</p>
		<p>
			The command that actually runs the sweep is <code>tga audit</code>, documented below. It has
			its own page here because it is a deliverable in its own right — someone reading a
			due-diligence report wants that report, not a git-analytics crate.
		</p>

		<h2>The gaps are named, not filled in</h2>
		<p>
			A stage that fails does not stop the sweep; it becomes a named line in the report's Gaps &amp;
			Caveats instead of a zero in a cell. Point the sweep at a config with no JIRA project key and
			the <code>jira sync</code> stage fails — that failure is recorded as a gap, not silently rendered
			as "no JIRA activity".
		</p>
	</div>
</section>

<section class="border-y border-foundry-border bg-foundry-raised">
	<div class="mx-auto max-w-content px-4 py-16 sm:px-6">
		<div class="prose-block">
			<h2 class="!mt-0"><code>tga audit</code> — the ten stages</h2>
			<p>
				One non-interactive command runs the whole pipeline across every repository the config
				names, in this order:
			</p>
			<div class="doc-table">
				<table>
					<thead>
						<tr>
							<th>#</th>
							<th>Stage</th>
						</tr>
					</thead>
					<tbody>
						{#each stages as stage, i (stage)}
							<tr>
								<td class="font-mono text-xs">{i + 1}</td>
								<td><code>{stage}</code></td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>

			<h3>Flags</h3>
			<p>
				All seven are optional; a bare <code>tga audit</code> with no flags still runs and renders.
			</p>
			<div class="doc-table">
				<table>
					<thead>
						<tr>
							<th>Flag</th>
							<th>What it does</th>
						</tr>
					</thead>
					<tbody>
						{#each flags as row (row.flag)}
							<tr>
								<td><code>{row.flag}</code></td>
								<td>{row.note}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
			<p class="text-sm">
				Source: <a
					href={AUDIT_SOURCE_URL}
					rel="noreferrer noopener"
					class="text-foundry-primary underline underline-offset-2">src/audit/</a
				> in this repository.
			</p>
		</div>
	</div>
</section>

<section class="mx-auto max-w-content px-4 py-16 sm:px-6">
	<div class="prose-block">
		<h2 id="install" class="scroll-mt-24 !mt-0">What you need installed</h2>
		<p>
			Two binaries. <code>tga</code> collects and classifies; <code>trusty-review</code> renders the
			report at the end of the sweep by running <code>trusty-review report</code> as a subprocess. They
			meet at a file — the manifest — not at a Cargo dependency edge, so the renderer is a separate install.
		</p>
		<CommandBlock
			command={'cargo install tga --locked\ncargo install trusty-review --locked'}
			label="Copy install commands"
		/>
		<p>
			<code>trusty-review</code> 0.15.0 or newer is required — an older copy is rejected before the
			first stage runs, with the upgrade command, rather than delivering a report with no written
			analysis. The sweep looks for <code>trusty-review</code> on <code>PATH</code>; set
			<code>TRUSTY_REVIEW_BIN</code> to a full path if it lives somewhere PATH cannot see it.
		</p>
		<p>
			The renderer writes the report's analysis with a model, so an audit cannot finish without a
			credential. It is checked before the first stage runs, not at the end:
		</p>
		<CommandBlock command="export OPENROUTER_API_KEY=…" label="Copy credential export" />
	</div>
</section>

<section class="border-y border-foundry-border bg-foundry-raised">
	<div class="mx-auto max-w-content px-4 py-16 sm:px-6">
		<div class="prose-block">
			<h2 class="!mt-0">The <code>trusty-audit</code> binary itself</h2>
			<p>
				<code>crates/trusty-audit/install.sh</code> downloads a release tarball, verifies it against
				its published SHA-256 checksum, installs it into
				<code>$CARGO_HOME/bin</code> (or <code>~/.cargo/bin</code>) with an atomic rename, and
				launches it. It runs on macOS Apple Silicon only — no Intel Mac or Linux asset is published
				for this crate, and the script refuses rather than handing you a binary that cannot execute.
			</p>
			<p>
				<strong class="font-semibold text-foundry-text">Current state:</strong> the script itself
				now lives in this repository, but it still resolves <em>releases</em> from the pre-split
				<code>bobmatnyc/trusty-tools</code> repository — it hard-codes
				<code>REPO="bobmatnyc/trusty-tools"</code>, which is where <code>trusty-audit</code>
				shipped before this repository split off. It will need repointing at this repository's own Releases,
				and this repository will need to have cut a <code>trusty-audit</code>
				release, before the command below actually installs anything.
			</p>
			<CommandBlock
				command={`curl -fsSL https://raw.githubusercontent.com/bobmatnyc/trusty-git-analytics/main/crates/trusty-audit/install.sh | sh`}
				label="Copy trusty-audit install command"
			/>
			<p>Until then, build it from a checkout instead:</p>
			<CommandBlock
				command="git clone https://github.com/bobmatnyc/trusty-git-analytics\ncd trusty-git-analytics\ncargo build --release -p trusty-audit"
				label="Copy build-from-source command"
			/>
		</div>
	</div>
</section>

<section class="mx-auto max-w-content px-4 py-16 sm:px-6">
	<div class="card flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
		<div>
			<h2 class="font-display text-xl font-semibold">Read the source</h2>
			<p class="mt-1 text-sm text-foundry-secondary">
				trusty-audit publishes no documentation page yet — its own README and source are the
				reference.
			</p>
		</div>
		<a href={SOURCE_URL} rel="noreferrer noopener" class="btn btn-primary shrink-0">
			View on GitHub
		</a>
	</div>
</section>
