<script lang="ts">
	// The dataset reset (D-022, D-066): a fresh, empty dataset as a new epoch, confirmed by
	// typing the project's slug. Nothing is deleted. The request needs an admin key, this
	// origin and the session's CSRF token; the server checks all three, not this page.
	import { onMount } from 'svelte';
	import { Alert, AlertDescription, AlertTitle } from '@sandbox/ui/ui/alert';
	import { Button } from '@sandbox/ui/ui/button';
	import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@sandbox/ui/ui/card';
	import { Input } from '@sandbox/ui/ui/input';
	import { Label } from '@sandbox/ui/ui/label';
	import { Skeleton } from '@sandbox/ui/ui/skeleton';
	import { ApiError, get, post } from '#lib/api.js';
	import { session } from '#lib/session.svelte.js';

	interface Project {
		id: string;
		slug: string;
		name: string;
		dataset_epoch: number;
	}
	interface ResetAnswer {
		project: Project;
		reset: { from_epoch: number; to_epoch: number; archived_payments: number };
	}
	type Result =
		| { kind: 'done'; from: number; to: number; archived: number }
		| { kind: 'refused'; title: string; words: string; code: string; detail: string }
		| { kind: 'unknown'; expected: number };

	let project = $state<Project | null>(null);
	let loadError = $state<{ words: string; code: string } | null>(null);
	let typed = $state('');
	let busy = $state(false);
	let result = $state<Result | null>(null);

	const isAdmin = $derived(session.info?.scopes.includes('admin') ?? false);
	const matches = $derived(project !== null && typed === project.slug);

	async function load() {
		loadError = null;
		try {
			project = (await get<{ project: Project }>('/v1/project')).project;
			if (session.info) session.info.project.dataset_epoch = project.dataset_epoch;
		} catch (e) {
			const error = e instanceof ApiError ? e : new ApiError(0, 'failed', 'Loading failed.');
			loadError = { words: error.message, code: error.code };
		}
	}
	onMount(load);

	function refusal(error: ApiError): Result {
		switch (error.code) {
			case 'confirmation_mismatch':
				return {
					kind: 'refused',
					title: 'That is not this project’s slug',
					words: 'Nothing was reset. Type the slug exactly as shown, then try again.',
					code: error.code,
					detail: error.message
				};
			case 'epoch_changed':
				return {
					kind: 'refused',
					title: 'Someone reset this dataset already',
					words: 'Nothing more was reset. The page now shows the current epoch; reset again only if you still want a fresh one.',
					code: error.code,
					detail: error.message
				};
			case 'forbidden':
				return {
					kind: 'refused',
					title: 'This key may not reset',
					words: 'A reset needs a key with the admin scope. Sign out and sign in with an admin key.',
					code: error.code,
					detail: error.message
				};
			default:
				return {
					kind: 'refused',
					title: 'The reset was refused',
					words: 'Nothing was reset. Try again in a moment; if it keeps failing, check the server log.',
					code: error.code,
					detail: error.message
				};
		}
	}

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		if (!project || !matches || busy) return;
		busy = true;
		result = null;
		const expected = project.dataset_epoch;
		try {
			const answer = await post<ResetAnswer>('/v1/project/reset', { confirm: typed, epoch: expected });
			project = answer.project;
			if (session.info) session.info.project.dataset_epoch = answer.project.dataset_epoch;
			result = {
				kind: 'done',
				from: answer.reset.from_epoch,
				to: answer.reset.to_epoch,
				archived: answer.reset.archived_payments
			};
			typed = '';
		} catch (e) {
			const error = e instanceof ApiError ? e : new ApiError(0, 'failed', 'The reset failed.');
			if (error.status === 0) {
				// No answer: the reset may or may not have happened. Never guess.
				result = { kind: 'unknown', expected };
			} else {
				result = refusal(error);
				if (error.code === 'epoch_changed') await load();
			}
		} finally {
			busy = false;
		}
	}

	/** After an unknown outcome: read the epoch back and say what really happened. */
	async function checkUnknown(expected: number) {
		await load();
		if (!project) return;
		result =
			project.dataset_epoch > expected
				? { kind: 'done', from: expected, to: project.dataset_epoch, archived: -1 }
				: {
						kind: 'refused',
						title: 'The reset did not happen',
						words: `The dataset is still at epoch ${project.dataset_epoch}. You can reset again.`,
						code: 'not_applied',
						detail: 'The epoch read back is the one the reset named.'
					};
	}
</script>

<svelte:head><title>Dataset · Sandbox dashboard</title></svelte:head>

<h1 class="display-2 mb-2">Dataset</h1>
<p class="mb-6 max-w-2xl text-sm text-muted-foreground">
	Start over with an empty dataset. A reset opens a new <em>epoch</em>: payments, balances and explanations start
	from nothing, and the same idempotency keys can be used again. Nothing is deleted: every journal entry, payment and
	event of the earlier epoch stays in the ledger, archived.
</p>

{#if loadError}
	<Alert variant="destructive" class="mb-4 max-w-2xl">
		<AlertTitle>The project could not be loaded</AlertTitle>
		<AlertDescription>
			<p>{loadError.words}</p>
			<Button variant="outline" size="sm" class="mt-2" onclick={load}>Try again</Button>
		</AlertDescription>
	</Alert>
{:else if project === null}
	<div class="max-w-2xl space-y-3" aria-busy="true" aria-label="Loading the project">
		<Skeleton class="h-24 w-full rounded-2xl" />
		<Skeleton class="h-56 w-full rounded-2xl" />
	</div>
{:else}
	<div class="grid max-w-2xl gap-4">
		<Card>
			<CardContent class="text-sm">
				<dl class="grid grid-cols-[auto_1fr] gap-x-6 gap-y-2">
					<dt class="text-muted-foreground">Project</dt>
					<dd class="font-medium"><bdi>{project.name}</bdi></dd>
					<dt class="text-muted-foreground">Slug</dt>
					<dd class="font-mono">{project.slug}</dd>
					<dt class="text-muted-foreground">Current epoch</dt>
					<dd class="tabular font-medium" data-testid="epoch">{project.dataset_epoch}</dd>
				</dl>
			</CardContent>
		</Card>

		{#if result?.kind === 'done'}
			<Alert role="status" data-testid="reset-done">
				<AlertTitle>The dataset is empty again</AlertTitle>
				<AlertDescription>
					Epoch {result.to} is now current.
					{#if result.archived >= 0}
						{result.archived === 1 ? '1 payment' : `${result.archived} payments`} of epoch {result.from} stay archived in the ledger.
					{:else}
						Everything of epoch {result.from} stays archived in the ledger.
					{/if}
					<a class="underline underline-offset-4" href="/payments">See the (empty) payments</a>
				</AlertDescription>
			</Alert>
		{:else if result?.kind === 'refused'}
			<Alert variant="destructive" data-testid="reset-refused">
				<AlertTitle>{result.title}</AlertTitle>
				<AlertDescription>
					<p>{result.words}</p>
					<details class="mt-1">
						<summary class="cursor-pointer rounded-sm focus-visible:ring-4 focus-visible:ring-ring/25 focus-visible:outline-none">Details</summary>
						<p class="mt-1 font-mono text-xs">{result.code}: {result.detail}</p>
					</details>
				</AlertDescription>
			</Alert>
		{:else if result?.kind === 'unknown'}
			{@const expected = result.expected}
			<Alert variant="destructive" data-testid="reset-unknown">
				<AlertTitle>We could not tell whether the reset happened</AlertTitle>
				<AlertDescription>
					<p>The sandbox did not answer in time. Check before trying again: a second reset would open yet another epoch.</p>
					<Button variant="outline" size="sm" class="mt-2" onclick={() => checkUnknown(expected)}>Check what happened</Button>
				</AlertDescription>
			</Alert>
		{/if}

		<Card>
			<CardHeader>
				<CardTitle><h2 class="title">Reset to an empty dataset</h2></CardTitle>
				<CardDescription>
					Open epoch {project.dataset_epoch + 1}. Checkouts still open in epoch {project.dataset_epoch} close; payments of
					it can no longer be captured or refunded. Webhooks already owed are still delivered.
				</CardDescription>
			</CardHeader>
			<CardContent>
				{#if !isAdmin}
					<p class="mb-3 text-sm" data-testid="needs-admin">
						You are signed in with a key that cannot reset. Sign in with a key that has the <span class="font-mono">admin</span> scope.
					</p>
				{/if}
				<form class="space-y-3" onsubmit={submit}>
					<Label for="confirm">Type <span class="font-mono">{project.slug}</span> to confirm</Label>
					<Input
						id="confirm"
						autocomplete="off"
						autocapitalize="off"
						spellcheck={false}
						disabled={!isAdmin || busy}
						bind:value={typed}
						aria-describedby="confirm-hint"
					/>
					<p id="confirm-hint" class="text-xs text-muted-foreground">
						{#if typed === '' || matches}
							The slug, exactly as shown above.
						{:else}
							Not the slug yet.
						{/if}
					</p>
					<Button type="submit" variant="destructive" disabled={!isAdmin || !matches || busy} aria-busy={busy}>
						{busy ? 'Resetting…' : `Reset and open epoch ${project.dataset_epoch + 1}`}
					</Button>
				</form>
			</CardContent>
		</Card>
	</div>
{/if}
