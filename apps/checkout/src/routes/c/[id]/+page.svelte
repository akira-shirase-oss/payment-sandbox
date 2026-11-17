<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { Button } from '@sandbox/ui/ui/button';
	import { Card, CardContent, CardHeader, CardTitle } from '@sandbox/ui/ui/card';
	import { formatMinor } from '@sandbox/ui/money';
	import { UNREACHABLE } from '@sandbox/ui/outcome';
	import { getView, pay, Refused, resume, Unreachable, type View } from '#lib/api.js';
	import {
		attemptSettledWithoutMoney,
		clearPending,
		isSessionId,
		loadPending,
		newAttemptKey,
		pollDelay,
		safeReturnUrl,
		savePending,
		step,
		takeSecret
	} from '#lib/flow.js';
	import ScenarioPicker from '#lib/ScenarioPicker.svelte';
	import Status from '#lib/Status.svelte';

	const id = page.params['id'] ?? '';
	let secret: string | null = null;
	let view = $state<View | null>(null);
	type Phase = 'loading' | 'ready' | 'submitting' | 'unreachable' | 'missing' | 'refused';
	let phase = $state<Phase>('loading');
	let refusal = $state('');
	let selected = $state('tok_sim_approve');
	let polls = 0;
	let continues = 0;
	let timer: ReturnType<typeof setTimeout> | undefined;
	let announced = '';

	const payable = $derived(view !== null && view.session.next_action === 'pay' && phase === 'ready');
	const returnUrl = $derived(view ? safeReturnUrl(view.session.return_url) : null);
	const finished = $derived(view !== null && step(view) === 'done');
	// Amounts are formatted here from integer minor units and the exponent, grouped.
	const amount = $derived(view ? formatMinor(view.session.amount, view.session.exponent) : '');

	onMount(() => {
		secret = isSessionId(id) ? takeSecret(id, location.hash) : null;
		if (!secret) {
			phase = 'missing';
			return;
		}
		void refresh();
		return () => clearTimeout(timer);
	});

	async function refresh() {
		if (!secret) return;
		try {
			apply(await getView(id, secret));
		} catch (error) {
			fail(error);
		}
	}

	function apply(next: View) {
		view = next;
		phase = 'ready';
		clearTimeout(timer);
		if (attemptSettledWithoutMoney(next)) clearPending(id);
		const s = step(next);
		if (s === 'wait') {
			timer = setTimeout(() => void refresh(), pollDelay(polls));
			polls += 1;
		} else {
			polls = 0;
		}
		if (s === 'continue' && continues < 5) {
			continues += 1;
			void advance();
		}
		if (s === 'done' || s === 'pay') tellParent(next);
	}

	function fail(error: unknown) {
		if (error instanceof Refused && (error.code === 'checkout_attempt_open' || error.code === 'checkout_closed')) {
			// Another press already started an attempt, or the session closed: show what is
			// recorded, never start a second charge.
			void refresh();
			return;
		}
		if (error instanceof Refused) {
			if (error.code === 'idempotency_key_conflict') clearPending(id);
			refusal = error.message;
			phase = error.status === 401 ? 'missing' : 'refused';
			return;
		}
		if (error instanceof Unreachable) {
			phase = 'unreachable';
			return;
		}
		throw error;
	}

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		if (phase === 'submitting' || !secret) return;
		const pending = loadPending(id) ?? { key: newAttemptKey(), token: selected };
		savePending(id, pending);
		phase = 'submitting';
		try {
			apply(await pay(id, secret, pending.key, pending.token));
		} catch (error) {
			fail(error);
		}
	}

	async function advance() {
		if (!secret) return;
		try {
			apply(await resume(id, secret));
		} catch (error) {
			fail(error);
		}
	}

	async function retry() {
		if (!secret) return;
		const pending = loadPending(id);
		phase = 'submitting';
		try {
			apply(pending ? await pay(id, secret, pending.key, pending.token) : await getView(id, secret));
		} catch (error) {
			fail(error);
		}
	}

	/** An embedded checkout tells its parent the outcome, addressed to each origin the
	 *  session allows to embed it, never to `*` (D-061). */
	function tellParent(next: View) {
		if (window.parent === window || announced === next.session.state) return;
		announced = next.session.state;
		for (const origin of next.session.embed_origins) {
			window.parent.postMessage(
				{ type: 'payment-sandbox.checkout', session: id, state: next.session.state, simulated: true },
				origin
			);
		}
	}
</script>

<svelte:head>
	<title>{view ? `Pay ${view.session.merchant}` : 'Sandbox checkout'} · simulated money only</title>
</svelte:head>

{#if phase === 'missing'}
	<Card>
		<CardHeader><CardTitle><h1 class="title">This checkout link is not valid</h1></CardTitle></CardHeader>
		<CardContent class="text-sm text-muted-foreground">
			<p>Open the full link the store gave you (it ends with <span class="font-mono">#cs_sim_…</span>).</p>
		</CardContent>
	</Card>
{:else if view === null && phase !== 'unreachable'}
	<p class="text-sm text-muted-foreground" aria-live="polite">Loading the checkout…</p>
{:else}
	<Card>
		<CardHeader class="gap-1">
			{#if view}
				<p class="eyebrow"><bdi>{view.session.merchant}</bdi></p>
				<CardTitle>
					<h1 class="display-2 figure" data-testid="amount">
						{amount}
						<span class="text-muted-foreground">{view.session.currency}</span>
					</h1>
				</CardTitle>
				{#if view.session.description}
					<p class="text-sm text-muted-foreground"><bdi>{view.session.description}</bdi></p>
				{/if}
			{:else}
				<CardTitle><h1 class="title">Sandbox checkout</h1></CardTitle>
			{/if}
		</CardHeader>
		<CardContent class="space-y-6">
			<section aria-live="polite" aria-atomic="true" aria-label="Payment status">
				{#if phase === 'unreachable'}
					<Status {view} override={UNREACHABLE} />
				{:else}
					<Status {view} />
				{/if}
			</section>

			{#if phase === 'unreachable'}
				<Button onclick={retry} class="w-full">Retry safely</Button>
			{:else if phase === 'refused'}
				<p role="alert" class="text-sm text-destructive">{refusal}</p>
			{/if}

			{#if view && (payable || phase === 'submitting') && view.session.next_action === 'pay'}
				<form class="space-y-4" onsubmit={submit}>
					<ScenarioPicker scenarios={view.scenarios} bind:selected disabled={phase === 'submitting'} />
					<Button type="submit" size="lg" class="w-full" disabled={phase === 'submitting'}>
						{phase === 'submitting' ? 'Submitting…' : `Pay ${amount} ${view.session.currency}`}
					</Button>
					<p class="text-center text-xs text-muted-foreground">
						{view.session.attempts_left} attempts left. Nothing here is real money.
					</p>
				</form>
			{/if}

			{#if finished && returnUrl}
				<Button href={returnUrl} variant="outline" class="w-full">Return to <bdi>{view?.session.merchant}</bdi></Button>
			{/if}
		</CardContent>
	</Card>
{/if}
