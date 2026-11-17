<script lang="ts">
	import { page } from '$app/state';
	import { OutcomeBadge } from '@sandbox/ui/components/sandbox/index.js';
	import { deliveryOutcome, operationOutcome, paymentOutcome } from '@sandbox/ui/outcome';
	import { Card, CardContent, CardHeader, CardTitle } from '@sandbox/ui/ui/card';
	import * as Table from '@sandbox/ui/ui/table';
	import { get, type Entry, type EventRecord, type Operation, type Payment } from '#lib/api.js';
	import { account, money, short, when } from '#lib/format.js';

	let payment = $state<Payment | null>(null);
	let operations = $state<Operation[]>([]);
	let entries = $state<Entry[]>([]);
	let events = $state<EventRecord[]>([]);
	let error = $state('');

	$effect(() => {
		const id = page.params['id'] ?? '';
		void (async () => {
			try {
				// The payment, its operations and its journal entries from one snapshot (D-076).
				const found = await get<{ payment: Payment; operations: Operation[]; entries: Entry[] }>(
					`/v1/payments/${encodeURIComponent(id)}/journal`
				);
				payment = found.payment;
				operations = found.operations;
				entries = found.entries;
				events = (await get<{ events: EventRecord[] }>(`/v1/events?payment_id=${encodeURIComponent(id)}&limit=200`)).events;
			} catch (e) {
				error = e instanceof Error ? e.message : 'Loading failed.';
			}
		})();
	});

	const sums = $derived(
		payment
			? [
					['Authorized', payment.authorized],
					['Captured', payment.captured],
					['Voided', payment.voided],
					['Refunded', payment.refunded],
					['Still capturable', payment.capturable],
					['Still refundable', payment.refundable],
					['Reserved by operations in flight', payment.reserved.capture + payment.reserved.void + payment.reserved.refund]
				] as const
			: []
	);
</script>

<svelte:head><title>Payment · Sandbox dashboard</title></svelte:head>

{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
{#if payment}
	<div class="mb-6 space-y-2">
		<p class="eyebrow"><a href="/payments" class="underline">Payments</a> / <span class="font-mono">{payment.id}</span></p>
		<div class="flex flex-wrap items-center gap-3">
			<h1 class="display-2 figure">{money(payment.amount, payment.currency)}</h1>
			<OutcomeBadge outcome={paymentOutcome(payment.status)} />
		</div>
		<p class="text-sm text-muted-foreground">
			{paymentOutcome(payment.status).detail} Scenario <span class="font-mono">{payment.token}</span> · {when(payment.created_at)}
			{#if payment.description} · <bdi>{payment.description}</bdi>{/if}
		</p>
	</div>
	<div class="grid gap-4 lg:grid-cols-[20rem_1fr]">
		<Card>
			<CardHeader><CardTitle><h2 class="title">Hold</h2></CardTitle></CardHeader>
			<CardContent>
				<dl class="grid grid-cols-[1fr_auto] gap-x-4 gap-y-1 text-sm" data-testid="payment-sums">
					{#each sums as [label, value] (label)}
						<dt>{label}</dt>
						<dd class="tabular text-right">{money(value, payment.currency)}</dd>
					{/each}
				</dl>
			</CardContent>
		</Card>
		<Card class="min-w-0">
			<CardHeader><CardTitle><h2 class="title">Operations</h2></CardTitle></CardHeader>
			<CardContent class="overflow-x-auto">
				<Table.Root>
					<Table.Header>
						<Table.Row><Table.Head>Kind</Table.Head><Table.Head>State</Table.Head><Table.Head>Amount</Table.Head><Table.Head>Key</Table.Head><Table.Head>Calls</Table.Head></Table.Row>
					</Table.Header>
					<Table.Body>
						{#each operations as op (op.id)}
							<Table.Row>
								<Table.Cell>{op.kind}</Table.Cell>
								<!-- The state first: at 360 px the table scrolls sideways, and what happened must be in view. -->
								<Table.Cell><OutcomeBadge outcome={operationOutcome(op.state)} />{#if op.decline_code}<span class="ml-2 text-xs text-muted-foreground">{op.decline_code}</span>{/if}</Table.Cell>
								<Table.Cell class="tabular">{money(op.amount, payment.currency)}</Table.Cell>
								<Table.Cell class="max-w-56 truncate font-mono text-xs" title={op.idempotency_key}>{op.idempotency_key}</Table.Cell>
								<Table.Cell class="tabular">{op.attempts}</Table.Cell>
							</Table.Row>
						{/each}
					</Table.Body>
				</Table.Root>
			</CardContent>
		</Card>
	</div>
	<div class="mt-4 grid gap-4 lg:grid-cols-2">
		<Card class="min-w-0">
			<CardHeader><CardTitle><h2 class="title">Journal entries</h2></CardTitle></CardHeader>
			<CardContent class="space-y-4 text-sm">
				{#each entries as entry (entry.id)}
					<div>
						<p class="font-medium">#{entry.seq} {entry.recipe} <span class="text-muted-foreground">· {new Date(entry.posted_at).toLocaleString()}</span></p>
						<ul class="mt-1 space-y-0.5">
							{#each entry.lines as [acct, amount], i (i)}
								<li class="flex justify-between gap-4">
									<a class="underline decoration-border" href={`/ledger/${entry.currency}/${acct}`}>{account(acct)}</a>
									<span class="tabular">{money(amount, entry.currency)}</span>
								</li>
							{/each}
						</ul>
					</div>
				{:else}
					<p class="text-muted-foreground">Nothing posted yet.</p>
				{/each}
			</CardContent>
		</Card>
		<Card class="min-w-0">
			<CardHeader><CardTitle><h2 class="title">Events and deliveries</h2></CardTitle></CardHeader>
			<CardContent class="space-y-3 text-sm">
				{#each events as event (event.id)}
					<div>
						<p class="font-medium">{event.sequence}. {event.kind} <span class="font-mono text-xs text-muted-foreground">{short(event.id)}</span></p>
						<div class="mt-1 flex flex-wrap gap-2">
							{#each event.deliveries as [endpoint, state, attempts] (endpoint)}
								<span class="inline-flex items-center gap-2"><OutcomeBadge outcome={deliveryOutcome(state)} /><span class="text-xs text-muted-foreground">to {short(endpoint)}, {attempts} attempts</span></span>
							{:else}
								<span class="text-xs text-muted-foreground">No endpoint was registered.</span>
							{/each}
						</div>
					</div>
				{:else}
					<p class="text-muted-foreground">No events yet.</p>
				{/each}
			</CardContent>
		</Card>
	</div>
{/if}
