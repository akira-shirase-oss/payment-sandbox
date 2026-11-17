<script lang="ts">
	import { onMount } from 'svelte';
	import { OutcomeBadge } from '@sandbox/ui/components/sandbox/index.js';
	import { paymentOutcome } from '@sandbox/ui/outcome';
	import { Button } from '@sandbox/ui/ui/button';
	import { get, type Payment } from '#lib/api.js';
	import { money, short, when } from '#lib/format.js';
	import VirtualRows from '#lib/VirtualRows.svelte';
	import LoadError from '#lib/LoadError.svelte';
	import { problem, type Problem } from '#lib/problem.js';

	const PAGE = 200;
	let rows = $state<Payment[]>([]);
	let next = $state<string | null>(null);
	let done = $state(false);
	let loading = $state(false);
	let error = $state<Problem | null>(null);

	async function more() {
		if (loading || done) return;
		loading = true;
		error = null;
		try {
			const before = next ? `&before=${encodeURIComponent(next)}` : '';
			const page = await get<{ payments: Payment[]; next_before: string | null }>(`/v1/payments?limit=${PAGE}${before}`);
			rows = [...rows, ...page.payments];
			next = page.next_before;
			done = next === null;
		} catch (e) {
			error = problem(e, 'the payments');
		} finally {
			loading = false;
		}
	}
	onMount(more);
</script>

<svelte:head><title>Payments · Sandbox dashboard</title></svelte:head>

<div class="mb-4 flex flex-wrap items-end justify-between gap-2">
	<h1 class="display-2">Payments</h1>
	<p class="text-sm text-muted-foreground" data-testid="payments-count">
		{rows.length} loaded{done ? '' : ', more on scroll'}
	</p>
</div>
{#if error}<LoadError problem={error} retry={() => void more()} />{/if}
{#if rows.length === 0 && done}
	<p class="text-sm text-muted-foreground" data-testid="payments-empty">
		No payments in this epoch yet. Make one from your store, with
		<span class="font-mono">sandbox-cli payments create</span>, or by running a library scenario
		(<span class="font-mono">sandbox-cli scenarios run purchase</span>).
	</p>
{:else}
	<VirtualRows items={rows} rowHeight={64} viewport={560} label="Payments, newest first" onnearend={more}>
		{#snippet row(p: Payment)}
			<a
				href={`/payments/${p.id}`}
				title={p.id}
				class="grid h-full grid-cols-[1fr_auto] items-center gap-x-3 px-4 hover:bg-accent focus-visible:bg-accent focus-visible:outline-none sm:grid-cols-[8rem_1fr_9rem_auto]"
			>
				<span class="figure truncate font-medium sm:order-2">{money(p.amount, p.currency)}</span>
				<span class="justify-self-end sm:order-4"><OutcomeBadge outcome={paymentOutcome(p.status)} /></span>
				<span class="truncate font-mono text-xs text-muted-foreground sm:order-1">{short(p.id)}</span>
				<span class="truncate text-xs text-muted-foreground sm:order-3">{when(p.created_at)}</span>
			</a>
		{/snippet}
	</VirtualRows>
	{#if !done}
		<Button variant="outline" size="sm" class="mt-3" disabled={loading} onclick={more}>Load more</Button>
	{/if}
{/if}
