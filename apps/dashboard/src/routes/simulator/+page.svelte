<script lang="ts">
	// The simulated network's test controls (D-063): only in the dev profile, and only with
	// the operator's own credential, which stays in this tab's memory and is forwarded to the
	// network with each request. A dashboard session alone cannot change the network.
	import { Button } from '@sandbox/ui/ui/button';
	import { Card, CardContent, CardHeader, CardTitle } from '@sandbox/ui/ui/card';
	import { Input } from '@sandbox/ui/ui/input';
	import { Label } from '@sandbox/ui/ui/label';
	import { get, post } from '#lib/api.js';
	import { session } from '#lib/session.svelte.js';

	let operator = $state('');
	let now = $state<number | null>(null);
	let seconds = $state(3600);
	let kind = $state('authorize');
	let action = $state('lost_reply');
	let count = $state(1);
	let message = $state('');
	let failed = $state(false);

	const headers = () => ({ 'x-operator-token': operator });

	async function run(label: string, work: () => Promise<unknown>) {
		message = '';
		failed = false;
		try {
			await work();
			message = label;
		} catch (e) {
			failed = true;
			message = e instanceof Error ? e.message : 'The control failed.';
		}
	}
	const readClock = () =>
		run('Clock read.', async () => {
			now = (await get<{ network: { now_seconds: number } }>('/sim/clock', headers())).network.now_seconds;
		});
	const advance = () =>
		run(`Clock moved by ${seconds} s.`, async () => {
			now = (await post<{ network: { now_seconds: number } }>('/sim/clock', { seconds }, headers())).network.now_seconds;
		});
	const addFault = () =>
		run(`Fault added: the next ${count} ${kind} ${action.replace('_', ' ')}.`, () =>
			post('/sim/faults', { kind, action, count }, headers())
		);
	const clear = () => run('Every fault cleared.', () => post('/sim/faults/clear', {}, headers()));
</script>

<svelte:head><title>Simulator · Sandbox dashboard</title></svelte:head>

<h1 class="display-2 mb-2">Simulator</h1>
{#if !session.info?.sim_controls}
	<p class="text-sm text-muted-foreground">The simulator's controls exist only when the server runs in the dev profile.</p>
{:else}
	<p class="mb-6 text-sm text-muted-foreground">
		Faults and the virtual clock of the simulated network. Each control needs the operator credential
		(<span class="font-mono">PS_SIM_CONTROL_TOKEN</span>); the network checks it, not the dashboard.
	</p>
	<div class="grid gap-4 md:grid-cols-2">
		<Card class="md:col-span-2">
			<CardContent class="space-y-2 pt-6">
				<Label for="operator">Operator credential</Label>
				<Input id="operator" type="password" autocomplete="off" spellcheck={false} bind:value={operator} />
			</CardContent>
		</Card>
		<Card>
			<CardHeader><CardTitle><h2 class="title">Virtual clock</h2></CardTitle></CardHeader>
			<CardContent class="space-y-3 text-sm">
				<p>Network time: <span class="tabular font-medium">{now === null ? 'not read yet' : new Date(now * 1000).toISOString()}</span></p>
				<p class="text-muted-foreground">Moving it never changes posted journal history (drill b).</p>
				<div class="flex flex-wrap items-end gap-2">
					<div class="space-y-1"><Label for="seconds">Seconds</Label><Input id="seconds" type="number" class="w-36" bind:value={seconds} /></div>
					<Button variant="outline" disabled={!operator} onclick={readClock}>Read</Button>
					<Button disabled={!operator} onclick={advance}>Move</Button>
				</div>
			</CardContent>
		</Card>
		<Card>
			<CardHeader><CardTitle><h2 class="title">Faults</h2></CardTitle></CardHeader>
			<CardContent class="space-y-3 text-sm">
				<fieldset class="space-y-1">
					<legend class="font-medium">Operation</legend>
					{#each ['authorize', 'capture', 'void', 'refund'] as k (k)}
						<label class="mr-3 inline-flex items-center gap-1.5"><input type="radio" name="kind" value={k} bind:group={kind} />{k}</label>
					{/each}
				</fieldset>
				<fieldset class="space-y-1">
					<legend class="font-medium">What the network does</legend>
					{#each [['lost_reply', 'executes, then loses the reply'], ['unavailable', 'answers 503 without executing']] as [a, words] (a)}
						<label class="flex items-center gap-1.5"><input type="radio" name="action" value={a} bind:group={action} />{words}</label>
					{/each}
				</fieldset>
				<div class="flex flex-wrap items-end gap-2">
					<div class="space-y-1"><Label for="count">Times</Label><Input id="count" type="number" min="1" max="1000" class="w-24" bind:value={count} /></div>
					<Button disabled={!operator} onclick={addFault}>Add fault</Button>
					<Button variant="outline" disabled={!operator} onclick={clear}>Clear faults</Button>
				</div>
			</CardContent>
		</Card>
	</div>
	{#if message}
		<p role={failed ? 'alert' : 'status'} class="mt-4 text-sm" class:text-destructive={failed}>{message}</p>
	{/if}
{/if}
