<script lang="ts">
	import { cn, type WithElementRef } from "#lib/utils.js";
	import type { HTMLTableAttributes } from "svelte/elements";

	// `label` names the table's scroll region. A table wider than its column scrolls
	// sideways inside its container; a labelled, focusable region lets a keyboard user
	// scroll it and tells a screen-reader user what it holds (WCAG 2.1.1, axe
	// scrollable-region-focusable). Every table in the apps passes one.
	let {
		ref = $bindable(null),
		class: className,
		label,
		children,
		...restProps
	}: WithElementRef<HTMLTableAttributes> & { label: string } = $props();
</script>

<!-- A scroll region must take focus to be scrolled from the keyboard (axe
     scrollable-region-focusable); it is labelled and has role="region", so it is announced. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div
	data-slot="table-container"
	role="region"
	aria-label={label}
	tabindex="0"
	class="relative w-full overflow-x-auto rounded-[inherit] outline-none focus-visible:ring-4 focus-visible:ring-ring/25"
>
	<table bind:this={ref} data-slot="table" class={cn("w-full caption-bottom text-sm", className)} {...restProps}>
		{@render children?.()}
	</table>
</div>
