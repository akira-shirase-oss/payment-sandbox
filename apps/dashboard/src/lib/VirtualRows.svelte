<script lang="ts" generics="T">
	// A list that renders only the rows in view (plus a margin), for anything that can grow
	// past a few hundred rows: payments, explanation lines. Rows have one fixed height, so
	// the visible slice is arithmetic; sizes are set through the CSSOM (style: directives),
	// which the sites' policy allows (no inline style attributes, D-061).
	import type { Snippet } from 'svelte';

	let {
		items,
		rowHeight,
		viewport = 480,
		overscan = 6,
		label,
		row,
		onnearend
	}: {
		items: T[];
		rowHeight: number;
		viewport?: number;
		overscan?: number;
		label: string;
		row: Snippet<[T, number]>;
		onnearend?: () => void;
	} = $props();

	let scrollTop = $state(0);
	let height = $state(0);
	const shown = $derived(height > 0 ? height : viewport);
	const first = $derived(Math.max(0, Math.floor(scrollTop / rowHeight) - overscan));
	const last = $derived(Math.min(items.length, Math.ceil((scrollTop + shown) / rowHeight) + overscan));
	const visible = $derived(items.slice(first, last));

	$effect(() => {
		if (onnearend && items.length > 0 && last >= items.length - overscan) onnearend();
	});
</script>

<!-- A scrolling region must be reachable by keyboard (WCAG 2.1.1), so it takes focus. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div
	class="overflow-y-auto overscroll-contain rounded-xl border bg-card focus-visible:ring-4 focus-visible:ring-ring/25 focus-visible:outline-none"
	style:max-height={`${viewport}px`}
	bind:clientHeight={height}
	onscroll={(event) => (scrollTop = event.currentTarget.scrollTop)}
	role="list"
	aria-label={label}
	data-testid="virtual-rows"
	data-total={items.length}
	data-rendered={visible.length}
	tabindex="0"
>
	<div class="relative" style:height={`${items.length * rowHeight}px`}>
		<div class="absolute inset-x-0 top-0" style:transform={`translateY(${first * rowHeight}px)`}>
			{#each visible as item, i (first + i)}
				<div role="listitem" class="border-b last:border-b-0" style:height={`${rowHeight}px`}>
					{@render row(item, first + i)}
				</div>
			{/each}
		</div>
	</div>
</div>
