import { execFileSync } from 'node:child_process';
import { createServer } from 'node:http';
import { existsSync, readFileSync, rmSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import type { AddressInfo } from 'node:net';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { chromium, type Browser } from 'playwright';

/**
 * Why: `tests/build-smoke.test.ts` proves the production build's HTML
 * contains the right text, but jsdom (the `unit` project's environment) has
 * no layout engine — it cannot compute `scrollWidth`, so it cannot see a page
 * scroll sideways. This is that measurement: a real Chromium loads the
 * PRODUCTION build's static output and reads `scrollWidth`/`clientWidth` the
 * same way a phone would, at 375px and 320px.
 * What: serves `.vercel/output/static` from a plain Node HTTP server (the
 * adapter's clean-URL convention — `/install` -> `install.html`,
 * `/docs/usage` -> `docs/usage.html`) and asserts no page's `<html>` scrolls
 * horizontally at either width. 320px is deliberately narrower than the
 * 375px minimum this task requires, so a fix tuned to exactly one width
 * cannot pass by luck.
 * Test: this file.
 */

const HERE = path.dirname(fileURLToPath(import.meta.url));
const WEBSITE_ROOT = path.resolve(HERE, '..');
const OUTPUT = path.join(WEBSITE_ROOT, '.vercel/output');
const STATIC = path.join(OUTPUT, 'static');

const ROUTES = ['/', '/install', '/docs/usage', '/trusty-audit'];
const WIDTHS = [375, 320];

/** `/` -> `index.html`; `/a/b` -> `a/b.html`. */
function routeToFile(route: string): string {
	if (route === '/') return 'index.html';
	return `${route.slice(1)}.html`;
}

const CONTENT_TYPES: Record<string, string> = {
	'.html': 'text/html',
	'.css': 'text/css',
	'.js': 'text/javascript',
	'.woff2': 'font/woff2',
	'.svg': 'image/svg+xml',
	'.json': 'application/json'
};

/**
 * Why: a route that only maps clean URLs to their HTML file (the adapter's
 * own convention) serves a page with every CSS/JS subresource 404ing — the
 * page then renders in the browser's UA stylesheet, not Foundry's, which
 * changes `scrollWidth` and has nothing to do with the defect this file
 * measures. Static assets under `_app/`, `fonts/`, etc. are served from
 * their literal path FIRST; only an extension-less request falls back to
 * the clean-URL HTML mapping.
 */
function resolveStaticFile(url: string): string | null {
	const clean = url.split('?')[0];
	const literal = path.join(STATIC, clean);
	if (existsSync(literal) && statSync(literal).isFile()) return literal;
	const mapped = path.join(STATIC, routeToFile(clean));
	if (existsSync(mapped)) return mapped;
	return null;
}

let server: ReturnType<typeof createServer>;
let baseUrl: string;
let browser: Browser;

beforeAll(async () => {
	// Independent of `build-smoke.test.ts`'s own build — Vitest gives each
	// test file no ordering guarantee, so this can't assume that file's
	// `beforeAll` already ran, or that `.vercel/output` is still the build it
	// left behind. `fileParallelism: false` in `vite.config.ts` keeps the two
	// smoke files' builds from racing on this same fixed path.
	rmSync(OUTPUT, { recursive: true, force: true });

	execFileSync('node', [path.join(WEBSITE_ROOT, 'node_modules/vite/bin/vite.js'), 'build'], {
		cwd: WEBSITE_ROOT,
		stdio: 'inherit',
		env: { ...process.env, NODE_ENV: 'production' }
	});

	server = createServer((req, res) => {
		const file = resolveStaticFile(req.url ?? '/');
		if (!file) {
			res.writeHead(404);
			res.end('not found');
			return;
		}
		const contentType = CONTENT_TYPES[path.extname(file)] ?? 'application/octet-stream';
		res.writeHead(200, { 'content-type': contentType });
		res.end(readFileSync(file));
	});
	await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
	const { port } = server.address() as AddressInfo;
	baseUrl = `http://127.0.0.1:${port}`;
	browser = await chromium.launch();
});

afterAll(async () => {
	await browser?.close();
	await new Promise<void>((resolve) => server.close(() => resolve()));
});

describe('no page scrolls horizontally on mobile', () => {
	for (const width of WIDTHS) {
		for (const route of ROUTES) {
			it(`${route} at ${width}px`, async () => {
				const page = await browser.newPage({ viewport: { width, height: 800 } });
				try {
					await page.goto(baseUrl + route, { waitUntil: 'networkidle' });
					const measurements = await page.evaluate(() => ({
						scrollWidth: document.documentElement.scrollWidth,
						clientWidth: document.documentElement.clientWidth
					}));
					expect(
						measurements.scrollWidth,
						`${route} at ${width}px: document scrollWidth ${measurements.scrollWidth} > clientWidth ${measurements.clientWidth}`
					).toBeLessThanOrEqual(measurements.clientWidth);
				} finally {
					await page.close();
				}
			});
		}
	}

	it('both themes render without a console error', async () => {
		for (const colorScheme of ['light', 'dark'] as const) {
			const page = await browser.newPage({ colorScheme });
			try {
				const errors: string[] = [];
				page.on('pageerror', (err) => errors.push(String(err)));
				await page.goto(baseUrl + '/', { waitUntil: 'networkidle' });
				expect(errors, `${colorScheme} theme: ${errors.join('; ')}`).toEqual([]);
			} finally {
				await page.close();
			}
		}
	});
});
