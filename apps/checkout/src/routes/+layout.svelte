<script lang="ts">
	import '../app.css';
	import { ModeWatcher } from 'mode-watcher';
	import { SandboxBanner, ThemeToggle } from '@sandbox/ui/components/sandbox/index.js';

	let { children } = $props();
</script>

<!-- The theme script is static in app.html (hashed by the server's policy), not injected;
     no transition-blocking <style> is injected on a switch either (style-src 'self'). -->
<ModeWatcher defaultMode="system" disableHeadScriptInjection disableTransitions={false} />

<div class="flex min-h-dvh flex-col bg-background text-foreground">
	<SandboxBanner what="This is a test checkout." />
	<header class="border-b">
		<div class="mx-auto flex h-14 w-full max-w-xl items-center justify-between px-4">
			<p class="font-semibold tracking-tight">Payment Sandbox <span class="text-muted-foreground">· checkout</span></p>
			<ThemeToggle />
		</div>
	</header>
	<main class="mx-auto w-full max-w-xl flex-1 px-4 py-6 sm:py-10">
		{@render children()}
	</main>
</div>
