<script lang="ts" module>
	import type { Tone } from '#lib/outcome.js';

	// Each tone has its own colour, icon and (from the outcome) word, so no state depends on
	// colour alone. Uncertain is always dashed: neither paid nor failed. The badge never
	// shrinks or clips its word.
	export const TONE_CLASSES: Record<Tone, string> = {
		ok: 'border-transparent bg-live-soft text-live-ink',
		held: 'border-transparent bg-held-soft text-held-ink',
		progress: 'border-transparent bg-pending-soft text-pending-ink',
		uncertain: 'border-dashed border-uncertain bg-uncertain-soft text-uncertain-ink',
		fail: 'border-transparent bg-attention-soft text-attention-ink',
		none: 'border-transparent bg-draft-soft text-draft-ink',
		refund: 'border-transparent bg-refund-soft text-refund-ink'
	};
</script>

<script lang="ts">
	import CircleCheck from '@lucide/svelte/icons/circle-check';
	import CircleHelp from '@lucide/svelte/icons/circle-help';
	import CircleSlash from '@lucide/svelte/icons/circle-slash';
	import CircleX from '@lucide/svelte/icons/circle-x';
	import Clock from '@lucide/svelte/icons/clock';
	import Lock from '@lucide/svelte/icons/lock';
	import Undo2 from '@lucide/svelte/icons/undo-2';
	import type { Outcome } from '#lib/outcome.js';
	import { cn } from '#lib/utils.js';

	let { outcome, class: className = '' }: { outcome: Outcome; class?: string } = $props();

	const ICONS = {
		ok: CircleCheck,
		held: Lock,
		progress: Clock,
		uncertain: CircleHelp,
		fail: CircleX,
		none: CircleSlash,
		refund: Undo2
	} as const;
	const Icon = $derived(ICONS[outcome.tone]);
</script>

<span
	data-slot="outcome"
	data-tone={outcome.tone}
	class={cn(
		'inline-flex w-max max-w-none shrink-0 items-center gap-1.5 whitespace-nowrap rounded-full border px-2.5 py-0.5 text-xs font-medium',
		TONE_CLASSES[outcome.tone],
		className
	)}
>
	<Icon aria-hidden="true" class="size-3.5 shrink-0" />
	<span>{outcome.word}</span>
</span>
