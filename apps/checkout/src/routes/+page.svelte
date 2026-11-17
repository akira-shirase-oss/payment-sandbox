<script lang="ts">
	import { Card, CardContent, CardHeader, CardTitle } from '@sandbox/ui/ui/card';
</script>

<Card>
	<CardHeader>
		<CardTitle><h1 class="title">No checkout here</h1></CardTitle>
	</CardHeader>
	<CardContent class="space-y-2 text-sm text-muted-foreground">
		<p>
			A test store opens this page with the link it got when it created a checkout session
			(<span class="font-mono">POST /v1/checkout_sessions</span>). The link looks like
			<span class="font-mono break-all">/c/&lt;session&gt;#cs_sim_…</span>.
		</p>
	</CardContent>
</Card>
