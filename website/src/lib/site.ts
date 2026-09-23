/**
 * Why: keeping the site's factual content in one typed module makes it
 * reviewable as data rather than as markup, and gives every claim a single
 * place to be checked against its source. Every string below was checked
 * against `Cargo.toml`, `src/main.rs`'s `Commands` enum, `src/audit/`, or
 * `crates/trusty-audit/` in the tga repository at the commit this site
 * shipped from — never against a README alone, several of which in this
 * repository predate the split from trusty-tools and still name the old
 * monorepo.
 *
 * What: navigation, external URLs, and the top-line facts rendered by the
 * home, install, and trusty-audit pages.
 *
 * Test: `src/lib/site.test.ts` checks the MSRV and license against
 * `Cargo.toml`, and the subcommand list against `src/main.rs`.
 */

export const GITHUB_URL = 'https://github.com/bobmatnyc/trusty-git-analytics';
export const CRATES_IO_URL = 'https://crates.io/crates/tga';

/**
 * Why: the canonical link and `og:url` on every page need one origin, set
 * once. Owner ruling (Bob): the Vercel project is `trusty-git-analytics`,
 * domain `tga.trustytools.dev` — see website/README.md, "Vercel setup".
 * What: the production origin. `+layout.svelte` appends the current route's
 * pathname to build each page's canonical URL.
 */
export const SITE_URL = 'https://tga.trustytools.dev';

export const NAV_LINKS: { href: string; label: string }[] = [
	{ href: '/', label: 'Home' },
	{ href: '/install', label: 'Install' },
	{ href: '/docs/usage', label: 'Usage' },
	{ href: '/trusty-audit', label: 'trusty-audit' }
];

export const FACTS: { label: string; value: string }[] = [
	{ label: 'Package', value: 'tga' },
	{ label: 'Binary', value: 'tga' },
	{ label: 'License', value: 'MIT' },
	{ label: 'MSRV', value: 'Rust 1.94' },
	{ label: 'Store', value: 'SQLite, on disk' },
	{ label: 'Output', value: 'CSV, JSON, Markdown' }
];

/**
 * One row per `tga` subcommand, in the order `src/main.rs`'s `Commands` enum
 * declares them. The description is a light paraphrase of that enum's own
 * doc comment, not new copy.
 */
export interface Subcommand {
	name: string;
	group: string;
	description: string;
}

export const SUBCOMMANDS: Subcommand[] = [
	{
		name: 'tga analyze',
		group: 'Pipeline',
		description: 'Run the full pipeline: collect → classify → report.'
	},
	{
		name: 'tga collect',
		group: 'Pipeline',
		description: 'Collect commits from git repositories into the database (Stage 1).'
	},
	{
		name: 'tga classify',
		group: 'Pipeline',
		description: 'Classify collected commits using the four-tier cascade (Stage 2).'
	},
	{
		name: 'tga report',
		group: 'Pipeline',
		description: 'Generate productivity reports from classified commits (Stage 3).'
	},
	{
		name: 'tga author',
		group: 'Reporting',
		description: 'Per-engineer drill-down report for a single canonical identity.'
	},
	{
		name: 'tga pr-metrics',
		group: 'Reporting',
		description: 'Aggregate pull-request metrics per engineer.'
	},
	{
		name: 'tga profile',
		group: 'Reporting',
		description: 'Longitudinal per-contributor quality profile.'
	},
	{
		name: 'tga dora',
		group: 'Reporting',
		description: 'Compute and display DORA metrics (lead time, deployment frequency, MTTR, CFR).'
	},
	{
		name: 'tga deployments',
		group: 'Reporting',
		description: 'DORA deployment-event ingestion and management.'
	},
	{
		name: 'tga incidents',
		group: 'Reporting',
		description: 'DORA incident ingestion and management.'
	},
	{
		name: 'tga audit',
		group: 'Reporting',
		description:
			'One-shot acquisition-diligence sweep over an org or configured repo set. See the trusty-audit page.'
	},
	{
		name: 'tga install',
		group: 'Setup',
		description: 'Interactive configuration wizard for first-time setup.'
	},
	{
		name: 'tga aliases',
		group: 'Setup',
		description: 'List, merge, or manage developer identity aliases.'
	},
	{
		name: 'tga backfill',
		group: 'Setup',
		description:
			'Retroactive maintenance: re-run ticket-id extraction, effort scoring, or reachability.'
	},
	{
		name: 'tga override',
		group: 'Setup',
		description: 'Manage manual classification overrides (Tier 0 of the cascade).'
	},
	{
		name: 'tga rules',
		group: 'Setup',
		description: 'Introspect or validate the active classification rule set.'
	},
	{
		name: 'tga inspect',
		group: 'Setup',
		description: 'Show the live database schema, or attest what it holds.'
	},
	{
		name: 'tga config',
		group: 'Setup',
		description: 'Manage inference provider configuration (API keys).'
	},
	{
		name: 'tga jira',
		group: 'External sync',
		description: 'JIRA status-transition and comment ingestion.'
	},
	{
		name: 'tga linear',
		group: 'External sync',
		description: 'Linear bulk team issue-set sync and freshness.'
	},
	{
		name: 'tga tui',
		group: 'Other',
		description: 'Interactive terminal UI: repo picker, live progress, correlation results.'
	}
];
