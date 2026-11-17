<script lang="ts">
	// What happened, truthfully: the state's own badge and sentence. "Paid" appears only for
	// the paid state; an unknown or uncertain outcome says so and what is safe to do.
	import { OutcomeBadge } from '@sandbox/ui/components/sandbox/index.js';
	import { checkoutOutcome, type Outcome } from '@sandbox/ui/outcome';
	import { formatMinor } from '@sandbox/ui/money';
	import type { View } from './api.js';

	let { view, override = null }: { view: View | null; override?: Outcome | null } = $props();

	const outcome = $derived(override ?? checkoutOutcome(view?.session.state));
	const DECLINES: Record<string, string> = {
		insufficient_funds: 'insufficient funds',
		do_not_honor: 'do not honor',
		expired_payment_method: 'expired payment method',
		network_identity_conflict: 'the same request key was used for another payment'
	};
	const reason = $derived(
		view?.attempt?.decline_code ? (DECLINES[view.attempt.decline_code] ?? view.attempt.decline_code) : null
	);
	const money = (minor: number) => (view ? `${formatMinor(minor, view.session.exponent)} ${view.session.currency}` : '');
</script>

<div class="space-y-2" data-testid="status" data-state={override ? 'unknown' : (view?.session.state ?? 'loading')}>
	<OutcomeBadge {outcome} />
	<p class="text-sm">{outcome.detail}</p>
	{#if !override && view?.session.state === 'declined' && reason}
		<p class="text-sm text-muted-foreground">Reason: {reason} (simulated).</p>
	{/if}
	{#if !override && view && view.refunded > 0}
		<p class="text-sm text-muted-foreground">
			{money(view.captured)} captured; {money(view.refunded)} refunded since.
		</p>
	{/if}
</div>
