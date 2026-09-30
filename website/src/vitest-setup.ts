/**
 * Why: Node 25 and later define their own `localStorage` and `sessionStorage`
 * globals (Web Storage, on by default), and those are `undefined` unless Node
 * runs with `--localstorage-file`. Vitest's jsdom environment leaves them in
 * place, so under Node 25+ the unit tests saw `localStorage === undefined`
 * where Node 20 hands them jsdom's Storage. A Node flag cannot fix it for
 * everyone: Node 20 rejects `--no-experimental-webstorage` as unknown.
 *
 * What: points both globals at the jsdom window's own Storage, so every Node
 * version runs the unit project against the same browser-shaped storage.
 * Vitest exposes that window as `globalThis.jsdom` in a jsdom environment.
 *
 * Test: `src/lib/theme/theme.test.ts`, which reads and writes localStorage in
 * every case.
 */

type JsdomGlobal = {
	jsdom?: { window: { localStorage: Storage; sessionStorage: Storage } };
};

const dom = (globalThis as typeof globalThis & JsdomGlobal).jsdom;
if (!dom) {
	throw new Error('vitest-setup.ts: no globalThis.jsdom; the unit project must use jsdom');
}

for (const key of ['localStorage', 'sessionStorage'] as const) {
	Object.defineProperty(globalThis, key, {
		value: dom.window[key],
		configurable: true,
		enumerable: true,
		writable: true
	});
}
