<script lang="ts">
	import { onMount } from 'svelte';
	import { OutcomeBadge } from '@sandbox/ui/components/sandbox/index.js';
	import { reconciliationOutcome } from '@sandbox/ui/outcome';
	import { Button } from '@sandbox/ui/ui/button';
	import { Card, CardContent, CardHeader, CardTitle } from '@sandbox/ui/ui/card';
	import { get, type Report, type TaskStatus } from '#lib/api.js';

	let report = $state<Report | null>(null);
	let tasks = $state<TaskStatus[]>([]);
	let error = $state('');
	let checking = $state(false);

	async function check(network = false) {
		checking = true;
		error = '';
		try {
			report = await get<Report>(`/v1/reconciliation${network ? '?network=true' : ''}`);
			tasks = (await get<{ tasks: TaskStatus[] }>('/status')).tasks;
		} catch (e) {
			error = e instanceof Error ? e.message : 'The check failed.';
		} finally {
			checking = false;
		}
	}
	onMount(() => void check());

	const outcome = $derived(report ? reconciliationOutcome(report.clean, report.network?.complete ?? true) : null);
</script>

<svelte:head><title>Overview · Sandbox dashboard</title></svelte:head>

<h1 class="display-2 mb-6">Overview</h1>
{#if error}<p role="alert" class="mb-4 text-sm text-destructive">{error}</p>{/if}
<div class="grid gap-4 md:grid-cols-2">
	<Card>
		<CardHeader><CardTitle><h2 class="title">Reconciliation</h2></CardTitle></CardHeader>
		<CardContent class="space-y-3 text-sm">
			{#if report && outcome}
				<div class="flex flex-wrap items-center gap-2" data-testid="reconciliation">
					<OutcomeBadge {outcome} />
					<span class="text-muted-foreground">{outcome.detail}</span>
				</div>
				<dl class="grid grid-cols-[1fr_auto] gap-x-4 gap-y-1">
					<dt>Payments checked</dt><dd class="tabular text-right">{report.report.payments_checked}</dd>
					<dt>Journal entries checked</dt><dd class="tabular text-right">{report.report.entries_checked}</dd>
					<dt>Open operations (pending or uncertain)</dt>
					<dd class="tabular text-right" data-testid="open-operations">{report.report.open_operations}</dd>
				</dl>
				{#if report.report.open_operations > 0}
					<p class="text-muted-foreground">
						Open operations are not failures: the reconciler reads the network's record back and resolves them.
					</p>
				{/if}
				{#each report.report.imbalanced_currencies as [currency, residue] (currency)}
					<p class="text-destructive">{currency} lines do not sum to zero (residue {residue}).</p>
				{/each}
				{#each report.report.payment_mismatches as id (id)}
					<p class="text-destructive">Payment <a class="underline" href={`/payments/${id}`}>{id}</a> differs from its journal.</p>
				{/each}
				{#each report.report.account_mismatches as [currency, acct, journal, implied] (`${currency}/${acct}`)}
					<p class="text-destructive">{currency} {acct}: journal {journal}, payments imply {implied}.</p>
				{/each}
				{#if report.network}
					<p class="text-muted-foreground">
						Network agreement {report.network.complete ? 'checked to completion' : 'stopped early: not known to be clean'}.
					</p>
				{/if}
			{:else}
				<p class="text-muted-foreground">Checking…</p>
			{/if}
			<div class="flex flex-wrap gap-2">
				<Button variant="outline" size="sm" disabled={checking} onclick={() => check(false)}>Check again</Button>
				<Button variant="outline" size="sm" disabled={checking} onclick={() => check(true)}>Check against the network</Button>
			</div>
		</CardContent>
	</Card>
	<Card>
		<CardHeader><CardTitle><h2 class="title">Background tasks</h2></CardTitle></CardHeader>
		<CardContent class="space-y-2 text-sm">
			{#each tasks as task (task.name)}
				<div class="flex flex-wrap items-center justify-between gap-2">
					<span class="font-medium">{task.name}</span>
					<OutcomeBadge
						outcome={task.running && !task.stale
							? { tone: 'ok', word: 'Running', detail: '' }
							: { tone: 'fail', word: task.stale ? 'Stuck' : 'Down', detail: '' }}
					/>
					<span class="w-full text-muted-foreground">
						Last beat {Math.round(task.since_beat_ms / 1000)} s ago · {task.restarts} restarts{task.last_failure
							? ` · last failure: ${task.last_failure}`
							: ''}
					</span>
				</div>
			{:else}
				<p class="text-muted-foreground">No background tasks reported.</p>
			{/each}
		</CardContent>
	</Card>
</div>
