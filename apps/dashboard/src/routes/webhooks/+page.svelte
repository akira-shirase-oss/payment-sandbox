<script lang="ts">
	import { onMount } from 'svelte';
	import { OutcomeBadge } from '@sandbox/ui/components/sandbox/index.js';
	import { deliveryOutcome, type Outcome } from '@sandbox/ui/outcome';
	import { Skeleton } from '@sandbox/ui/ui/skeleton';
	import * as Table from '@sandbox/ui/ui/table';
	import { get, type Endpoint, type EventRecord } from '#lib/api.js';
	import { short, when } from '#lib/format.js';
	import LoadError from '#lib/LoadError.svelte';
	import { problem, type Problem } from '#lib/problem.js';

	let endpoints = $state<Endpoint[]>([]);
	let events = $state<EventRecord[]>([]);
	let error = $state<Problem | null>(null);
	// Until both lists are in, a skeleton holds their place: no empty state that is not true
	// yet, and nothing below jumps when the rows arrive (CLS, perf/results web vitals).
	let loaded = $state(false);
	async function load() {
		error = null;
		try {
			const [e, ev] = await Promise.all([
				get<{ endpoints: Endpoint[] }>('/v1/webhook_endpoints?limit=100'),
				get<{ events: EventRecord[] }>('/v1/events?limit=100')
			]);
			endpoints = e.endpoints;
			events = ev.events;
			loaded = true;
		} catch (e) {
			error = problem(e, 'the webhooks');
		}
	}
	onMount(load);

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
{#if error}<LoadError problem={error} retry={load} />{/if}
{#if !loaded && !error}
	<div class="space-y-3" aria-busy="true" aria-label="Loading the webhooks">
		<Skeleton class="h-6 w-32" />
		<Skeleton class="h-28 w-full rounded-xl" />
		<Skeleton class="h-6 w-40" />
		<Skeleton class="h-64 w-full rounded-xl" />
	</div>
{:else if loaded}
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
				<Table.Row><Table.Cell colspan={4} class="text-muted-foreground">No endpoints registered. Add one with <code>sandbox-cli webhooks create --url http://127.0.0.1:&lt;port&gt;/webhooks</code>.</Table.Cell></Table.Row>
			{/each}
		</Table.Body>
	</Table.Root>
</div>
<h2 class="title mb-2">Recent events</h2>
<p class="mb-2 text-sm text-muted-foreground">Delivery is at least once: a receiver may see an event twice and must deduplicate it by its id.</p>
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
{/if}
