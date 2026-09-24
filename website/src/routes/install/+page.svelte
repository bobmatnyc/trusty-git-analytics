<script lang="ts">
	import CommandBlock from '$lib/components/CommandBlock.svelte';
	import PageMeta from '$lib/components/PageMeta.svelte';
	import { GITHUB_URL } from '$lib/site';
</script>

<PageMeta
	title="Install — tga"
	description="Install tga from crates.io with cargo, or download a prebuilt binary from the GitHub Releases page."
/>

<section class="border-b border-foundry-border">
	<div class="mx-auto max-w-content px-4 py-14 sm:px-6 sm:py-20">
		<p class="eyebrow">tga</p>
		<h1 class="mt-4 font-display text-4xl font-bold tracking-tight sm:text-5xl">Install</h1>
		<p class="mt-6 max-w-2xl text-lg text-foundry-secondary">
			Requires Rust 1.94 or later for a source build. No Rust toolchain is needed for a prebuilt
			binary.
		</p>
	</div>
</section>

<section class="mx-auto max-w-content px-4 py-16 sm:px-6">
	<div class="prose-block">
		<h2 class="!mt-0">With cargo, from crates.io</h2>
		<p>The <code>tga</code> package is published to crates.io.</p>
		<CommandBlock command="cargo install tga --locked" label="Copy cargo install command" />
		<p>
			<code>cargo install</code> writes the binary atomically to
			<code>~/.cargo/bin/</code> — make sure that directory is on your <code>PATH</code>.
		</p>

		<h2>From a GitHub release</h2>
		<p>
			Prebuilt binaries are attached to tagged releases on this repository's
			<a
				href={`${GITHUB_URL}/releases`}
				rel="noreferrer noopener"
				class="text-foundry-primary underline underline-offset-2">Releases page</a
			>. Download the archive for your platform, extract it, mark the binary executable, and place
			it on your <code>PATH</code>:
		</p>
		<CommandBlock
			command={'tar xzf tga-*.tar.gz\nchmod +x tga\nmv tga ~/.local/bin/  # or /usr/local/bin/'}
			label="Copy binary install commands"
		/>

		<h2>Verify</h2>
		<CommandBlock command="tga --version" label="Copy verify command" />

		<h2>Configure and run</h2>
		<p>
			<code>tga install</code> is an interactive wizard that writes a <code>config.yaml</code> for you.
			A hand-written one can be as small as a list of repository paths — every other section has a default.
		</p>
		<CommandBlock command={'tga install\ntga analyze'} label="Copy setup commands" />

		<h2>trusty-audit</h2>
		<p>
			trusty-audit is a separate crate in this repository, distributed as a self-downloading
			installer rather than through crates.io. See the
			<a href="/trusty-audit" class="text-foundry-primary underline underline-offset-2"
				>trusty-audit page</a
			>
			for the current state of its install path.
		</p>
	</div>
</section>
