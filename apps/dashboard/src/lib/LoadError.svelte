<script lang="ts">
	// A load that failed: what happened in plain words, what to do next (back to a list,
	// try again), and the code behind a details toggle.
	import { Alert, AlertDescription, AlertTitle } from '@sandbox/ui/ui/alert';
	import { Button } from '@sandbox/ui/ui/button';
	import type { Problem } from './problem.js';

	let {
		problem,
		retry,
		back
	}: { problem: Problem; retry?: () => void; back?: { href: string; label: string } } = $props();
</script>

<Alert variant="destructive" class="mb-4 max-w-2xl" data-testid="load-error">
	<AlertTitle>{problem.title}</AlertTitle>
	<AlertDescription>
		<p>{problem.words}</p>
		{#if back || retry}
			<div class="mt-2 flex flex-wrap gap-2">
				{#if back}<Button variant="outline" size="sm" href={back.href}>{back.label}</Button>{/if}
				{#if retry}<Button variant="outline" size="sm" onclick={retry}>Try again</Button>{/if}
			</div>
		{/if}
		<details class="mt-2">
			<summary class="cursor-pointer rounded-sm focus-visible:ring-4 focus-visible:ring-ring/25 focus-visible:outline-none">Details</summary>
			<p class="mt-1 font-mono text-xs">{problem.code}: {problem.detail}</p>
		</details>
	</AlertDescription>
</Alert>
