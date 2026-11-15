<script lang="ts">
	import { m } from '#lib/paraglide/messages.js';
	import { tick } from 'svelte';
	import { get } from '#lib/api.svelte.js';

	interface Logs {
		last: number;
		lines: [number, string][];
	}

	// The board keeps the last 16 KiB of its log; this page follows it.
	let lines = $state<[number, string][]>([]);
	let error = $state('');
	let follow = $state(true);
	let box: HTMLPreElement | undefined = $state();

	$effect(() => {
		let last = 0;
		let stopped = false;
		let timer: ReturnType<typeof setTimeout> | undefined;
		const fetchMore = async () => {
			try {
				const r = await get<Logs>(`/logs?after=${last}`);
				// The board restarted: its numbering starts over.
				if (r.last < last) lines = [];
				if (r.lines.length) lines = [...lines, ...r.lines].slice(-2000);
				last = r.last;
				error = '';
				if (follow) {
					await tick();
					box?.scrollTo({ top: box.scrollHeight });
				}
			} catch (e) {
				error = (e as Error).message;
			}
			if (!stopped) timer = setTimeout(fetchMore, 2000);
		};
		fetchMore();
		return () => {
			stopped = true;
			clearTimeout(timer);
		};
	});

	// After the timestamp (uptime, or local time once synced).
	const level = (line: string) => / (ERROR|WARN|INFO|DEBUG|TRACE) /.exec(line)?.[1].toLowerCase() ?? '';
</script>

<div class="row head">
	<h1>{m.log()}</h1>
	<label class="check"><input type="checkbox" bind:checked={follow} /> {m.follow()}</label>
</div>
{#if error}<p class="error">{error}</p>{/if}
<pre class="card" bind:this={box}>{#each lines as [n, line] (n)}<span class={level(line)}>{line}</span>
{/each}</pre>

<style>
	.head {
		justify-content: space-between;
	}
	.head label {
		margin: 0;
	}
	pre {
		height: 70vh;
		overflow: auto;
		font-size: 0.8rem;
		line-height: 1.4;
		margin: 0;
		white-space: pre-wrap;
		word-break: break-word;
	}
	.warn {
		color: #c27c0e;
	}
	.error {
		color: var(--warn);
	}
	.debug,
	.trace {
		color: var(--muted);
	}
</style>
