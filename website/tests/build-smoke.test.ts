import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { beforeAll, describe, expect, it } from 'vitest';
import { SITE_URL } from '../src/lib/site';

/**
 * Why: every other test here reads source files. None of them prove the site
 * actually BUILDS, and a Vercel deploy failure is the expensive way to find
 * that out. This runs the real production build once and asserts the
 * artefacts a Vercel deploy depends on — the Build Output API layout, the
 * prerendered pages, and the self-hosted fonts.
 * What: shells out to `vite build`, then inspects `.vercel/output/`. Slow,
 * which is why it lives in its own `smoke` Vitest project with a long
 * timeout rather than alongside the unit tests.
 * Test: this file.
 */

const HERE = path.dirname(fileURLToPath(import.meta.url));
const WEBSITE_ROOT = path.resolve(HERE, '..');
const OUTPUT = path.join(WEBSITE_ROOT, '.vercel/output');
const STATIC = path.join(OUTPUT, 'static');

const PAGES = ['index.html', 'install.html', 'docs/usage.html', 'trusty-audit.html'];

let landingPage = '';

/** Every built client-side JS chunk, concatenated — used to check for a stray third-party origin. */
function clientBundle(): string {
	const chunks: string[] = [];
	const walk = (dir: string) => {
		if (!existsSync(dir)) return;
		for (const entry of readdirSync(dir, { withFileTypes: true })) {
			const full = path.join(dir, entry.name);
			if (entry.isDirectory()) walk(full);
			else if (entry.name.endsWith('.js')) chunks.push(readFileSync(full, 'utf8'));
		}
	};
	walk(path.join(STATIC, '_app'));
	return chunks.join('\n');
}

beforeAll(() => {
	rmSync(OUTPUT, { recursive: true, force: true });
	execFileSync('node', [path.join(WEBSITE_ROOT, 'node_modules/vite/bin/vite.js'), 'build'], {
		cwd: WEBSITE_ROOT,
		stdio: 'inherit',
		env: { ...process.env, NODE_ENV: 'production' }
	});
	landingPage = readFileSync(path.join(STATIC, 'index.html'), 'utf8');
});

describe('production build', () => {
	it('emits the Vercel Build Output API layout', () => {
		expect(existsSync(path.join(OUTPUT, 'config.json'))).toBe(true);
		expect(existsSync(STATIC)).toBe(true);
	});

	it('prerenders every page to static HTML', () => {
		for (const page of PAGES) {
			expect(existsSync(path.join(STATIC, page)), page).toBe(true);
		}
	});

	it('renders real landing-page content, not a shell', () => {
		expect(landingPage).toContain('tga');
		expect(landingPage).toContain('Three stages, one command');
		expect(landingPage).toContain('trusty-audit');
		expect(landingPage).toContain('/install');
		expect(landingPage).toContain('/docs/usage');
	});

	it('links the repository and crates.io from the home page', () => {
		expect(landingPage).toContain('https://github.com/bobmatnyc/trusty-git-analytics');
		expect(landingPage).toContain('https://crates.io/crates/tga');
	});

	it('emits a canonical link and og:url pointing at tga.trustytools.dev', () => {
		const pages: [string, string][] = [
			['/', landingPage],
			['/trusty-audit', readFileSync(path.join(STATIC, 'trusty-audit.html'), 'utf8')]
		];
		for (const [route, html] of pages) {
			const canonical = `${SITE_URL}${route}`;
			expect(html, `${route} canonical link`).toContain(
				`<link rel="canonical" href="${canonical}"`
			);
			expect(html, `${route} og:url`).toContain(`<meta property="og:url" content="${canonical}"`);
			expect(html, `${route} og:site_name`).toContain(
				'<meta property="og:site_name" content="tga.trustytools.dev"'
			);
		}
	});

	it("emits og:title and og:description matching each page's title and description", () => {
		const pages: [string, string][] = [
			['/', 'index.html'],
			['/install', 'install.html'],
			['/docs/usage', 'docs/usage.html'],
			['/trusty-audit', 'trusty-audit.html']
		];
		for (const [route, file] of pages) {
			const html = readFileSync(path.join(STATIC, file), 'utf8');
			const title = /<title>([^<]*)<\/title>/.exec(html)?.[1];
			const description = /<meta name="description" content="([^"]*)"/.exec(html)?.[1];
			expect(title, `${route} <title>`).toBeTruthy();
			expect(description, `${route} meta description`).toBeTruthy();

			const ogTitle = /<meta property="og:title" content="([^"]*)"/.exec(html)?.[1];
			const ogDescription = /<meta property="og:description" content="([^"]*)"/.exec(html)?.[1];
			expect(ogTitle, `${route} og:title`).toBe(title);
			expect(ogDescription, `${route} og:description`).toBe(description);

			expect(html, `${route} og:type`).toContain('<meta property="og:type" content="website"');
			expect(html, `${route} twitter:card`).toContain(
				'<meta name="twitter:card" content="summary"'
			);
			expect(html, `${route} og:image`).not.toContain('og:image');
		}
	});

	it('renders the install page with the cargo and binary paths', () => {
		const html = readFileSync(path.join(STATIC, 'install.html'), 'utf8');
		expect(html).toContain('cargo install tga --locked');
		expect(html).toContain('/releases');
	});

	it('renders the usage page with every subcommand group', () => {
		const html = readFileSync(path.join(STATIC, 'docs/usage.html'), 'utf8');
		for (const group of ['Pipeline', 'Reporting', 'External sync', 'Setup', 'Other']) {
			expect(html, group).toContain(group);
		}
		expect(html).toContain('tga analyze');
		expect(html).toContain('tga audit');
	});

	it('renders the trusty-audit page with the ten stages and the root install.sh', () => {
		const html = readFileSync(path.join(STATIC, 'trusty-audit.html'), 'utf8');
		expect(html).toContain('cargo install trusty-review --locked');
		expect(html).toContain('OPENROUTER_API_KEY');
		expect(html).toContain(
			'https://raw.githubusercontent.com/bobmatnyc/trusty-git-analytics/main/install.sh'
		);
		expect(html).toContain('trusty-audit-v');
		expect(html).toContain('jira sync');
		expect(html, 'still names the pre-split crates/ path').not.toContain(
			'crates/trusty-audit/install.sh'
		);
	});

	it('names no fabricated crates.io claim for trusty-audit', () => {
		const html = readFileSync(path.join(STATIC, 'trusty-audit.html'), 'utf8');
		expect(html).not.toContain('cargo install trusty-audit');
	});

	// `rel="canonical"` links are excluded: they are a reference to this
	// site's own URL (SITE_URL), not a resource the page fetches, so an
	// absolute href there is correct rather than a leak.
	it('loads no subresource from a third-party origin', () => {
		for (const page of PAGES) {
			const html = readFileSync(path.join(STATIC, page), 'utf8');
			const linkHrefs = [...html.matchAll(/<link\b[^>]*>/g)]
				.filter((tag) => !/\brel="canonical"/.test(tag[0]))
				.map((tag) => /\bhref="([^"]+)"/.exec(tag[0])?.[1])
				.filter((href): href is string => Boolean(href));
			const subresources = [
				...html.matchAll(/<(?:script|img|source|iframe)\b[^>]*\bsrc="([^"]+)"/g)
			].map((match) => match[1]);
			const offSite = [...subresources, ...linkHrefs].filter((url) =>
				/^(?:https?:)?\/\//.test(url)
			);
			expect(offSite, `${page} loads ${offSite.join(', ')}`).toEqual([]);
		}
	});

	it('ships the self-hosted fonts and references no external font host', () => {
		expect(existsSync(path.join(STATIC, 'fonts/ibm-plex-sans-var.woff2'))).toBe(true);
		expect(existsSync(path.join(STATIC, 'fonts/OFL-IBMPlexSans.txt'))).toBe(true);
		expect(landingPage).not.toContain('fonts.googleapis.com');
		expect(landingPage).not.toContain('fonts.gstatic.com');
		expect(clientBundle()).not.toContain('fonts.googleapis.com');
	});

	it('sets the theme class before first paint', () => {
		// The anti-flash snippet must survive the build inlined in the HTML.
		expect(landingPage).toContain("classList.toggle('dark'");
	});
});
