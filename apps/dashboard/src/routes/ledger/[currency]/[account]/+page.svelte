<script lang="ts">
	// One balance explained line by line (the ship gate's explain-balance): the server pages
	// the postings of one snapshot with sealed cursors (D-042, D-056); the lines of every page,
	// summed, must equal the balance it reports.
	import { untrack } from 'svelte';
	import { page } from '$app/state';
	import { OutcomeBadge } from '@sandbox/ui/components/sandbox/index.js';
	import { Button } from '@sandbox/ui/ui/button';
	import { get, type ExplainedLine, type Explanation } from '#lib/api.js';
	import { account, money, short } from '#lib/format.js';
	import VirtualRows from '#lib/VirtualRows.svelte';
	import LoadError from '#lib/LoadError.svelte';
	import { problem, type Problem } from '#lib/problem.js';

	const currency = $derived(page.params['currency'] ?? '');
	const code = $derived(page.params['account'] ?? '');
	const limit = $derived(Math.min(500, Math.max(1, Number(page.url.searchParams.get('limit')) || 100)));

	let lines = $state<ExplainedLine[]>([]);
	let cursor = $state<string | null>(null);
	let balance = $state<number | null>(null);
	let lineCount = $state(0);
	let pages = $state(0);
	let done = $state(false);
	let loading = $state(false);
	let error = $state<Problem | null>(null);

	async function more() {
		if (loading || done) return;
		loading = true;
		error = null;
		try {
			const next = cursor ? `&cursor=${encodeURIComponent(cursor)}` : '';
			const answer = await get<Explanation>(
				`/v1/balances/${encodeURIComponent(currency)}/${encodeURIComponent(code)}/explain?limit=${limit}${next}`
			);
			lines = [...lines, ...answer.explanation.lines];
			balance = answer.explanation.balance_minor;
			lineCount = answer.explanation.line_count;
			cursor = answer.explanation.next?.cursor ?? null;
			pages += 1;
			done = cursor === null;
		} catch (e) {
			error = problem(e, 'this explanation');
			// A cursor from before a restore, or not this server's, cannot continue: start over.
			if (error.code === 'stale_cursor' || error.code === 'invalid_cursor') {
				error = { ...error, title: 'The explanation has to start again', words: 'The database changed under it (a restore), so its place in the journal is gone. Start again from the first page.' };
			}
		} finally {
			loading = false;
		}
	}

	// A new account (or page size) starts a new explanation; nothing else re-runs this.
	$effect(() => {
		void currency;
		void code;
		void limit;
		untrack(() => {
			lines = [];
			cursor = null;
			pages = 0;
			done = false;
			void more();
		});
	});

	const sum = $derived(lines.reduce((total, line) => total + line.amount_minor, 0));
	const verdict = $derived(
		!done
			? { tone: 'progress' as const, word: 'Partly explained', detail: '' }
			: sum === balance
				? { tone: 'ok' as const, word: 'Explained', detail: '' }
				: { tone: 'fail' as const, word: 'Does not add up', detail: '' }
	);
</script>

<svelte:head><title>{account(code)} · Ledger · Sandbox dashboard</title></svelte:head>

<p class="eyebrow mb-1"><a href="/ledger" class="underline">Ledger</a> / {currency}</p>
<h1 class="display-2 mb-2 capitalize">{account(code)}</h1>
{#if error}
	<LoadError
		problem={error}
		retry={error.code === 'stale_cursor' || error.code === 'invalid_cursor'
			? () => {
					lines = [];
					cursor = null;
					pages = 0;
					done = false;
					void more();
				}
			: () => void more()}
		back={{ href: '/ledger', label: 'Back to the ledger' }}
	/>
{/if}
{#if balance !== null}
	<div class="mb-4 flex flex-wrap items-center gap-3 text-sm" data-testid="explain-summary">
		<OutcomeBadge outcome={verdict} />
		<span>
			{lines.length} of {lineCount} lines in {pages} {pages === 1 ? 'page' : 'pages'} sum to
			<span class="tabular font-medium" data-testid="explained-sum">{money(sum, currency)}</span>; the journal balance is
			<span class="tabular font-medium" data-testid="journal-balance">{money(balance, currency)}</span>.
		</span>
	</div>
	<VirtualRows items={lines} rowHeight={52} viewport={520} label="Postings, oldest first">
		{#snippet row(line: ExplainedLine)}
			<div class="grid h-full grid-cols-[1fr_auto] items-center gap-x-3 px-4 text-sm sm:grid-cols-[5rem_1fr_9rem_9rem]">
				<span class="font-mono text-xs text-muted-foreground">#{line.seq}</span>
				<span class="min-w-0 truncate">
					{line.recipe}
					{#if line.payment_id}<a class="ml-1 font-mono text-xs underline decoration-border" href={`/payments/${line.payment_id}`}>{short(line.payment_id)}</a>{/if}
				</span>
				<span class="tabular text-right">{money(line.amount_minor, currency)}</span>
				<span class="tabular text-right text-muted-foreground">{money(line.running_minor, currency)}</span>
			</div>
		{/snippet}
	</VirtualRows>
	{#if !done}
		<Button variant="outline" size="sm" class="mt-3" disabled={loading} onclick={more}>Load the next page</Button>
	{/if}
{/if}
