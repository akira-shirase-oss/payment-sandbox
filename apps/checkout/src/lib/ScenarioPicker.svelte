<script lang="ts">
	// The payer's only choice (D-019): what the simulated network does. Native radio buttons,
	// so arrow keys move between them and Space selects, with nothing to type and no card
	// field anywhere.
	import type { Scenario } from './api.js';

	let {
		scenarios,
		selected = $bindable(),
		disabled = false
	}: { scenarios: Scenario[]; selected: string; disabled?: boolean } = $props();

	const KIND: Record<Scenario['kind'], string> = {
		approve: 'Approves',
		decline: 'Declines',
		uncertain: 'Answer lost'
	};
</script>

<fieldset class="space-y-2" {disabled}>
	<legend class="mb-2 text-sm font-medium">What should the simulated network do?</legend>
	{#each scenarios as scenario (scenario.token)}
		<label
			class="flex cursor-pointer items-center gap-3 rounded-xl border bg-card px-4 py-3 text-sm shadow-hairline transition-colors hover:bg-accent has-[:checked]:border-foreground has-[:disabled]:cursor-not-allowed has-[:disabled]:opacity-60"
		>
			<input
				type="radio"
				name="scenario"
				value={scenario.token}
				bind:group={selected}
				class="size-4 shrink-0 accent-primary focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
			/>
			<span class="min-w-0 flex-1">{scenario.label}</span>
			<span class="shrink-0 text-xs text-muted-foreground">{KIND[scenario.kind]}</span>
		</label>
	{/each}
</fieldset>
