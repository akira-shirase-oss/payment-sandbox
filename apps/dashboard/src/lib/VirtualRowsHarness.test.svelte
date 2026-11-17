<script lang="ts">
	import VirtualRows from './VirtualRows.svelte';
	let { items }: { items: string[] } = $props();
</script>

<VirtualRows {items} rowHeight={40} viewport={400} label="Test rows">
	{#snippet row(item: string)}<span>{item} </span>{/snippet}
</VirtualRows>
