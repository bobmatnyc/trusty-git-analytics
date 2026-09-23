import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { FACTS, SUBCOMMANDS } from './site';

/**
 * Why: `site.ts`'s doc comment claims every fact is checked against the
 * repository, not a README. This is that check, run against the repository
 * this site ships from rather than trusted to stay true by inspection alone.
 * What: re-derives the MSRV and license from the repository root `Cargo.toml`,
 * and the subcommand list from `src/main.rs`'s `Commands` enum, and asserts
 * `site.ts` agrees.
 */

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, '../../../');

function readRepoFile(relative: string): string {
	return readFileSync(path.join(REPO_ROOT, relative), 'utf8');
}

describe('site facts are grounded in the repository', () => {
	it('MSRV matches workspace.package.rust-version in Cargo.toml', () => {
		const cargoToml = readRepoFile('Cargo.toml');
		const match = /rust-version\s*=\s*"([\d.]+)"/.exec(cargoToml);
		expect(match, 'no rust-version found in Cargo.toml').not.toBeNull();
		const msrv = FACTS.find((f) => f.label === 'MSRV');
		expect(msrv?.value).toBe(`Rust ${match![1]}`);
	});

	it('license matches workspace.package.license in Cargo.toml', () => {
		const cargoToml = readRepoFile('Cargo.toml');
		const match = /^license\s*=\s*"([^"]+)"/m.exec(cargoToml);
		expect(match, 'no license found in Cargo.toml').not.toBeNull();
		const license = FACTS.find((f) => f.label === 'License');
		expect(license?.value).toBe(match![1]);
	});

	it('every subcommand named here exists in src/main.rs Commands enum', () => {
		const mainRs = readRepoFile('src/main.rs');
		for (const sub of SUBCOMMANDS) {
			// "tga analyze" -> "Analyze"; "tga pr-metrics" -> "PrMetrics".
			const verb = sub.name.replace(/^tga /, '');
			const variant = verb
				.split('-')
				.map((part) => part[0].toUpperCase() + part.slice(1))
				.join('');
			expect(mainRs, `${sub.name} (variant ${variant})`).toMatch(new RegExp(`\\b${variant}\\(`));
		}
	});
});
