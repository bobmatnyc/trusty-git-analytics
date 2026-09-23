<script lang="ts">
	/**
	 * Why: every command block on the install and trusty-audit pages owes the
	 * same two things — a `<pre>` that scrolls instead of widening the page,
	 * and a copy button with its own accessible name. Repeating that markup
	 * per block is how one of the two goes missing.
	 * What: the `<pre>` + `CopyButton` pair. `min-w-0` and `pr-14` are the
	 * containment fix: a `<pre>` never wraps, so without them the flex child
	 * widens to the longest command and the page scrolls sideways at 375px.
	 */
	import CopyButton from './CopyButton.svelte';

	interface Props {
		/** Exact text to render and copy — may be multi-line. */
		command: string;
		/** Accessible name for the copy button, e.g. "Copy install command". */
		label: string;
	}

	let { command, label }: Props = $props();
</script>

<div class="mt-4 max-w-3xl min-w-0">
	<div class="relative">
		<pre
			class="overflow-x-auto rounded-sm border border-foundry-border bg-foundry-card p-4 pr-14 text-xs leading-relaxed text-foundry-text">{command}</pre>
		<div class="absolute right-2 top-2">
			<CopyButton text={command} {label} />
		</div>
	</div>
</div>
