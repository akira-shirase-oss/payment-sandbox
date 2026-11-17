<script lang="ts">
	import '../app.css';
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { ModeWatcher } from 'mode-watcher';
	import { SandboxBanner, ThemeToggle } from '@sandbox/ui/components/sandbox/index.js';
	import { Button } from '@sandbox/ui/ui/button';
	import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@sandbox/ui/ui/card';
	import { Input } from '@sandbox/ui/ui/input';
	import { Label } from '@sandbox/ui/ui/label';
	import { load, session, signIn, signOut } from '#lib/session.svelte.js';

	let { children } = $props();
	let key = $state('');
	let error = $state('');
	let busy = $state(false);

	onMount(load);

	const NAV = [
		{ href: '/', label: 'Overview' },
		{ href: '/payments', label: 'Payments' },
		{ href: '/ledger', label: 'Ledger' },
		{ href: '/webhooks', label: 'Webhooks' },
		{ href: '/dataset', label: 'Dataset' }
	];
	const links = $derived(session.info?.sim_controls ? [...NAV, { href: '/simulator', label: 'Simulator' }] : NAV);
	const current = (href: string) =>
		href === '/' ? page.url.pathname === '/' : page.url.pathname.startsWith(href);

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		if (busy) return;
		busy = true;
		error = '';
		try {
			await signIn(key);
			key = '';
		} catch (e) {
			error = e instanceof Error ? e.message : 'Sign-in failed.';
		} finally {
			busy = false;
		}
	}
</script>

<!-- The theme script is static in app.html (hashed by the server's policy), not injected;
     no transition-blocking <style> is injected on a switch either (style-src 'self'). -->
<ModeWatcher defaultMode="system" disableHeadScriptInjection disableTransitions={false} />

<div class="flex min-h-dvh flex-col bg-background text-foreground">
	<SandboxBanner what="This dashboard inspects a sandbox." />
	<header class="border-b">
		<div class="mx-auto flex min-h-14 w-full max-w-6xl flex-wrap items-center justify-between gap-2 px-4 py-2 sm:px-6">
			<p class="font-semibold tracking-tight">
				Payment Sandbox
				{#if session.info}<span class="text-muted-foreground">· <bdi>{session.info.project.name}</bdi>
						· <a class="rounded-sm underline decoration-border underline-offset-4 hover:decoration-foreground focus-visible:ring-4 focus-visible:ring-ring/25 focus-visible:outline-none" href="/dataset" data-testid="header-epoch">epoch {session.info.project.dataset_epoch}</a></span>{/if}
			</p>
			<div class="flex items-center gap-1">
				<ThemeToggle />
				{#if session.status === 'signed-in'}
					<Button variant="outline" size="sm" onclick={signOut}>Sign out</Button>
				{/if}
			</div>
		</div>
		{#if session.status === 'signed-in'}
			<nav aria-label="Sections" class="mx-auto w-full max-w-6xl overflow-x-auto px-4 sm:px-6">
				<ul class="flex gap-1 pb-2">
					{#each links as link (link.href)}
						<li>
							<a
								href={link.href}
								aria-current={current(link.href) ? 'page' : undefined}
								class="inline-flex h-9 items-center rounded-full px-3 text-sm whitespace-nowrap hover:bg-accent focus-visible:ring-4 focus-visible:ring-ring/25 focus-visible:outline-none aria-[current=page]:bg-secondary aria-[current=page]:font-medium"
							>
								{link.label}
							</a>
						</li>
					{/each}
				</ul>
			</nav>
		{/if}
	</header>
	<main class="mx-auto w-full min-w-0 max-w-6xl flex-1 px-4 py-6 sm:px-6">
		{#if session.status === 'loading'}
			<p class="text-sm text-muted-foreground" aria-live="polite">Loading…</p>
		{:else if session.status === 'signed-out'}
			<Card class="mx-auto max-w-lg">
				<CardHeader>
					<CardTitle><h1 class="title">Sign in</h1></CardTitle>
					<CardDescription>
						Paste a project's sandbox key (<span class="font-mono">sk_sim_…</span>). It is sent once; the
						dashboard then holds a session of its own, which ends when the key is revoked.
					</CardDescription>
				</CardHeader>
				<CardContent>
					<form class="space-y-3" onsubmit={submit}>
						<Label for="key">Sandbox key</Label>
						<Input
							id="key"
							type="password"
							autocomplete="off"
							spellcheck={false}
							bind:value={key}
							placeholder="sk_sim_…"
							aria-invalid={error ? 'true' : undefined}
							aria-describedby={error ? 'key-error' : undefined}
						/>
						{#if error}<p id="key-error" role="alert" class="text-sm text-destructive">{error}</p>{/if}
						<Button type="submit" disabled={busy || key.trim() === ''}>Sign in</Button>
					</form>
				</CardContent>
			</Card>
		{:else}
			{@render children()}
		{/if}
	</main>
</div>
