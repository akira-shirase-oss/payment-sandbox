<script lang="ts">
	import { onMount } from 'svelte';
	import * as Table from '@sandbox/ui/ui/table';
	import { get, type Balance } from '#lib/api.js';
	import { account, money } from '#lib/format.js';
	import LoadError from '#lib/LoadError.svelte';
	import { problem, type Problem } from '#lib/problem.js';

	let balances = $state<Balance[]>([]);
	let error = $state<Problem | null>(null);
	async function load() {
		error = null;
		try {
			balances = (await get<{ balances: Balance[] }>('/v1/balances')).balances;
		} catch (e) {
			error = problem(e, 'the ledger');
		}
	}
	onMount(load);
</script>

<svelte:head><title>Ledger · Sandbox dashboard</title></svelte:head>

<h1 class="display-2 mb-2">Ledger</h1>
<p class="mb-6 text-sm text-muted-foreground">
	Every balance is summed from the journal. Open one to see the postings that make it up.
</p>
{#if error}<LoadError problem={error} retry={load} />{/if}
<!-- At 360 px the balance stays beside its account: currency and side fold under the name. -->
<div class="rounded-xl border bg-card">
	<Table.Root label="Balances">
		<Table.Header>
			<Table.Row><Table.Head>Account</Table.Head><Table.Head class="hidden sm:table-cell">Currency</Table.Head><Table.Head class="hidden sm:table-cell">Normal side</Table.Head><Table.Head class="text-right">Balance</Table.Head></Table.Row>
		</Table.Header>
		<Table.Body>
			{#each balances as b (`${b.currency}/${b.account}`)}
				<Table.Row>
					<Table.Cell>
						<a class="underline decoration-border" href={`/ledger/${b.currency}/${b.account}`}>{account(b.account)}</a>
						<span class="block text-xs text-muted-foreground sm:hidden">{b.currency} · {b.normal_side}</span>
					</Table.Cell>
					<Table.Cell class="hidden sm:table-cell">{b.currency}</Table.Cell>
					<Table.Cell class="hidden sm:table-cell">{b.normal_side}</Table.Cell>
					<Table.Cell class="tabular text-right whitespace-nowrap" data-testid="balance">{money(b.balance_minor, b.currency)}</Table.Cell>
				</Table.Row>
			{:else}
				<Table.Row><Table.Cell colspan={4} class="text-muted-foreground">No postings yet. Every payment posts here: make one from your store, or with <code>sandbox-cli payments create</code>.</Table.Cell></Table.Row>
			{/each}
		</Table.Body>
	</Table.Root>
</div>
