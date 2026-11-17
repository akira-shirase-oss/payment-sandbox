<script lang="ts">
	// The label every page carries (D-009): this is a sandbox, and its money is not real.
	let { what }: { what: string } = $props();
</script>

<div role="note" aria-label="Simulated money only" class="bg-primary text-primary-foreground">
	<p class="mx-auto max-w-6xl px-4 py-1.5 text-center text-xs font-medium sm:px-6">
		<strong class="font-semibold">SIMULATED MONEY ONLY.</strong>
		{what} No real cards, balances or banking connection.
	</p>
</div>
