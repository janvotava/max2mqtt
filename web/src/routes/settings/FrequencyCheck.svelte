<script lang="ts">
	import { get, post } from '#lib/api.svelte.js';

	interface Calibration {
		running: boolean;
		listening_at: number;
		seconds: number;
		packets: { from: string; rssi: number; offset: number }[];
		suggested: number | null;
	}

	/** `onuse` puts the measured value into the form. */
	let { value, onuse }: { value: number; onuse: (offset: number) => void } = $props();

	const KHZ = 1.587;
	let c = $state<Calibration | null>(null);
	let error = $state('');

	async function refresh() {
		try {
			c = await get<Calibration>('/radio/calibration');
			error = '';
		} catch (e) {
			error = (e as Error).message;
		}
	}

	// Poll while a measurement runs.
	$effect(() => {
		refresh();
		const timer = setInterval(() => c?.running && refresh(), 2000);
		return () => clearInterval(timer);
	});

	async function run(start: boolean) {
		await post('/radio/calibrate', { run: start }).catch((e) => (error = (e as Error).message));
		setTimeout(refresh, 300);
	}

	const khz = (steps: number) => `${steps >= 0 ? '+' : '−'}${Math.abs(steps * KHZ).toFixed(1)} kHz`;
	const enough = $derived((c?.packets.length ?? 0) >= 5);
	const spread = $derived(
		c && c.packets.length > 1 ? Math.max(...c.packets.map((p) => p.offset)) - Math.min(...c.packets.map((p) => p.offset)) : 0
	);
</script>

<div class="measure">
	{#if error}<p class="error">{error}</p>{/if}
	{#if c?.running || c?.packets.length}
		<p>
			{#if c.suggested !== null}
				Measured <strong>{c.suggested}</strong> ({khz(c.suggested)}) from {c.packets.length} packet{c.packets.length === 1
					? ''
					: 's'}{#if c.running}, still listening{/if}.
				{#if spread > 6}<span class="error">Readings disagree by {spread} steps; wait for more.</span>{/if}
			{:else}
				Listening for packets ({c.seconds} s{c.listening_at ? `, searching ${khz(c.listening_at)}` : ''}). Turn a thermostat
				wheel to send one.
			{/if}
		</p>
	{/if}
	<div class="row">
		{#if c?.running}
			<button type="button" class="secondary" onclick={() => run(false)}>Stop</button>
		{:else}
			<button type="button" class="secondary" onclick={() => run(true)}>{c?.packets.length ? 'Measure again' : 'Measure'}</button>
		{/if}
		{#if c && c.suggested !== null && c.suggested !== value}
			<button type="button" class="link" disabled={!enough} onclick={() => onuse(c!.suggested!)}>
				Use {c.suggested}{enough ? '' : ' (after 5 packets)'}
			</button>
		{/if}
	</div>
</div>

<style>
	.measure {
		margin: -0.25rem 0 0.75rem;
	}
	.measure p {
		margin: 0 0 0.5rem;
		font-size: 0.9rem;
	}
	.link {
		color: var(--accent);
	}
</style>
