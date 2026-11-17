<script lang="ts">
	import { onMount } from 'svelte';
	import { OutcomeBadge } from '@sandbox/ui/components/sandbox/index.js';
	import { deliveryOutcome, type Outcome } from '@sandbox/ui/outcome';
	import * as Table from '@sandbox/ui/ui/table';
	import { get, type Endpoint, type EventRecord } from '#lib/api.js';
	import { short, when } from '#lib/format.js';

	let endpoints = $state<Endpoint[]>([]);
	let events = $state<EventRecord[]>([]);
	let error = $state('');
	onMount(async () => {
		try {
			endpoints = (await get<{ endpoints: Endpoint[] }>('/v1/webhook_endpoints?limit=100')).endpoints;
			events = (await get<{ events: EventRecord[] }>('/v1/events?limit=100')).events;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Loading failed.';
		}
	});

	function health(e: Endpoint): Outcome {
		if (e.disabled_at) return { tone: 'none', word: 'Disabled', detail: '' };
		if (e.circuit_open_until && new Date(e.circuit_open_until) > new Date())
			return { tone: 'fail', word: 'Circuit open', detail: '' };
		if (e.consecutive_failures > 0) return { tone: 'uncertain', word: 'Failing', detail: '' };
		return { tone: 'ok', word: 'Healthy', detail: '' };
	}
</script>

<svelte:head><title>Webhooks · Sandbox dashboard</title></svelte:head>

<h1 class="display-2 mb-6">Webhooks</h1>
{#if error}<p role="alert" class="mb-4 text-sm text-destructive">{error}</p>{/if}
<h2 class="title mb-2">Endpoints</h2>
<div class="mb-8 rounded-xl border bg-card">
	<Table.Root label="Endpoints">
		<Table.Header>
			<Table.Row><Table.Head>URL</Table.Head><Table.Head>Health</Table.Head><Table.Head>Failures in a row</Table.Head><Table.Head>Secret</Table.Head></Table.Row>
		</Table.Header>
		<Table.Body>
			{#each endpoints as e (e.id)}
				<Table.Row>
					<Table.Cell class="max-w-72 truncate font-mono text-xs" title={e.url}>{e.url}</Table.Cell>
					<Table.Cell><OutcomeBadge outcome={health(e)} />{#if e.circuit_open_until}<span class="ml-2 text-xs text-muted-foreground">until {new Date(e.circuit_open_until).toLocaleTimeString()}</span>{/if}</Table.Cell>
					<Table.Cell class="tabular">{e.consecutive_failures}</Table.Cell>
					<Table.Cell>{e.rotating ? 'rotating (two secrets sign)' : 'one secret'}</Table.Cell>
				</Table.Row>
			{:else}
				<Table.Row><Table.Cell colspan={4} class="text-muted-foreground">No endpoints registered.</Table.Cell></Table.Row>
			{/each}
		</Table.Body>
	</Table.Root>
</div>
<h2 class="title mb-2">Recent events</h2>
<p class="mb-2 text-sm text-muted-foreground">Delivery is at least once: a receiver may see an event twice and must deduplicate (D-006).</p>
<div class="rounded-xl border bg-card">
	<Table.Root label="Recent events">
		<Table.Header>
			<Table.Row><Table.Head>Event</Table.Head><Table.Head>Payment</Table.Head><Table.Head>When</Table.Head><Table.Head>Deliveries</Table.Head></Table.Row>
		</Table.Header>
		<Table.Body>
			{#each events as ev (ev.id)}
				<Table.Row>
					<Table.Cell class="whitespace-nowrap">{ev.kind}</Table.Cell>
					<Table.Cell><a class="font-mono text-xs underline decoration-border" href={`/payments/${ev.payment_id}`}>{short(ev.payment_id)}</a></Table.Cell>
					<Table.Cell class="whitespace-nowrap text-xs">{when(ev.created_at)}</Table.Cell>
					<Table.Cell>
						<div class="flex flex-wrap gap-2">
							{#each ev.deliveries as [endpoint, state, attempts] (endpoint)}
								<span class="inline-flex items-center gap-1"><OutcomeBadge outcome={deliveryOutcome(state)} /><span class="text-xs text-muted-foreground">{attempts}×</span></span>
							{:else}
								<span class="text-xs text-muted-foreground">none</span>
							{/each}
						</div>
					</Table.Cell>
				</Table.Row>
			{:else}
				<Table.Row><Table.Cell colspan={4} class="text-muted-foreground">No events yet.</Table.Cell></Table.Row>
			{/each}
		</Table.Body>
	</Table.Root>
</div>
