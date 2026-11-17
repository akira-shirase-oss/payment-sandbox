<script lang="ts">
	// Light or dark, remembered by mode-watcher. Its head script is off: the same few lines
	// are static in each app's app.html, where the sites' policy allows them by hash (D-061).
	import Moon from '@lucide/svelte/icons/moon';
	import Sun from '@lucide/svelte/icons/sun';
	import { mode, toggleMode } from 'mode-watcher';
	import { Button } from '#lib/components/ui/button/index.js';

	const dark = $derived(mode.current === 'dark');
</script>

<Button
	variant="ghost"
	size="icon"
	aria-label={dark ? 'Switch to light mode' : 'Switch to dark mode'}
	aria-pressed={dark}
	onclick={toggleMode}
>
	{#if dark}
		<Sun aria-hidden="true" />
	{:else}
		<Moon aria-hidden="true" />
	{/if}
</Button>
