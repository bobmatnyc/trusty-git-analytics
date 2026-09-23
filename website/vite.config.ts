import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vitest/config';

/**
 * Why: the smoke suite shells out to a real `vite build` and then, for the
 * mobile-overflow check, drives a real Chromium — both slow relative to a
 * plain component/unit test, and neither belongs under jsdom. Splitting them
 * into their own Vitest project keeps the fast `unit` project fast.
 * What: two Vitest projects — `unit` (jsdom, `src/**`) and `smoke` (node,
 * `tests/**`, long timeout because it shells out to a real `vite build`).
 * `smoke` disables file parallelism: more than one `tests/**` file shells out
 * to `vite build` into the same fixed `.vercel/output`, and `adapter-vercel`
 * errors EEXIST symlinking a function's `node_modules` if a second build
 * starts before the first's is torn down.
 */
const SMOKE_TIMEOUT_MS = 300_000;

export default defineConfig({
	plugins: [sveltekit()],
	test: {
		projects: [
			{
				extends: true,
				// #5110 (trusty-tools) / same trap here: `mount()` only exists in
				// Svelte's CLIENT build. Vitest resolves packages under the `ssr`
				// conditions by default even in a jsdom environment, which hands a
				// test `svelte/index-server` and fails with `mount(...) is not
				// available on the server`.
				resolve: { conditions: ['browser'] },
				test: {
					name: 'unit',
					environment: 'jsdom',
					include: ['src/**/*.test.ts'],
					testTimeout: 30_000,
					hookTimeout: 30_000
				}
			},
			{
				extends: true,
				test: {
					name: 'smoke',
					environment: 'node',
					include: ['tests/**/*.test.ts'],
					testTimeout: SMOKE_TIMEOUT_MS,
					hookTimeout: SMOKE_TIMEOUT_MS,
					fileParallelism: false
				}
			}
		]
	}
});
