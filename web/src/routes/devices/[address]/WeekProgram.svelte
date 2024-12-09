<script lang="ts">
	import { untrack } from 'svelte';
	import { invalidate } from '$app/navigation';
	import { post } from '#lib/api.svelte.js';
	import { DAYS, type Day, type DeviceConfig } from '#lib/types.js';

	let { device }: { device: DeviceConfig } = $props();

	interface Point {
		time: string;
		temp: number;
	}
	type Week = Record<Day, Point[]>;

	const MAX_POINTS = 13;
	const label: Record<Day, string> = {
		monday: 'Mon',
		tuesday: 'Tue',
		wednesday: 'Wed',
		thursday: 'Thu',
		friday: 'Fri',
		saturday: 'Sat',
		sunday: 'Sun'
	};
	const minutes = (t: string) => {
		const [h, m] = t.split(':').map(Number);
		return h * 60 + m;
	};
	const pad = (n: number) => String(n).padStart(2, '0');
	const hhmm = (m: number) => `${pad(Math.floor(m / 60))}:${pad(m % 60)}`;
	const sorted = (points: Point[]) => [...points].sort((a, b) => minutes(a.time) - minutes(b.time));

	function load(d: DeviceConfig): Week {
		const fallback = (): Point[] => [
			{ time: '06:00', temp: d.temperatures.comfort },
			{ time: '22:00', temp: d.temperatures.eco }
		];
		return Object.fromEntries(
			DAYS.map((day) => {
				const stored = d.schedule?.[day];
				const points = stored ? Object.entries(stored).map(([time, temp]) => ({ time, temp })) : fallback();
				return [day, sorted(points)];
			})
		) as Week;
	}

	const initial = untrack(() => $state.snapshot(device));
	const known = Object.keys(initial.schedule ?? {}).length > 0;
	let week = $state<Week>(load(initial));
	let saved = $state(JSON.stringify(load(initial)));
	let open = $state<Day | null>(null);
	let busy = $state(false);
	let message = $state('');
	let error = $state('');

	const changed = $derived(JSON.stringify(week) !== saved);

	/** Blocks of one day for the bar: start, end (minutes) and temperature. */
	function blocks(points: Point[]) {
		const p = sorted(points);
		if (!p.length) return [];
		// Before the first switch point, the last one of the day still holds.
		const out = p[0].time === '00:00' ? [] : [{ from: 0, to: minutes(p[0].time), temp: p[p.length - 1].temp }];
		p.forEach((pt, k) => out.push({ from: minutes(pt.time), to: k + 1 < p.length ? minutes(p[k + 1].time) : 1440, temp: pt.temp }));
		return out.filter((b) => b.to > b.from);
	}

	// How strongly a temperature is tinted: "off" (4.5 °C) not at all, then 15–23 °C spread
	// over the range people actually use.
	const strength = (t: number) => (t <= 5 ? 0 : Math.round(25 + 75 * Math.min(1, Math.max(0, (t - 15) / 8))));
	const shade = (t: number) => `color-mix(in oklch, var(--accent) ${strength(t)}%, var(--line))`;

	function problems(points: Point[]): string | null {
		if (!points.length) return 'needs at least one switch point';
		if (points.length > MAX_POINTS) return `devices hold at most ${MAX_POINTS} switch points`;
		const times = points.map((p) => p.time);
		if (times.some((t) => !/^\d\d:\d\d$/.test(t) || minutes(t) % 5)) return 'times must be in 5 minute steps';
		if (new Set(times).size !== times.length) return 'two switch points at the same time';
		if (points.some((p) => !(p.temp >= 4.5 && p.temp <= 30.5))) return 'temperatures must be 4.5–30.5 °C';
		return null;
	}
	const invalid = $derived(DAYS.map((d) => [d, problems(week[d])] as const).filter(([, p]) => p));

	function add(day: Day) {
		const p = sorted(week[day]);
		const last = p.length ? minutes(p[p.length - 1].time) : 6 * 60;
		const time = hhmm(Math.min(23 * 60 + 55, last + 60));
		week[day] = sorted([...week[day], { time, temp: device.temperatures.comfort }]);
	}

	function copy(from: Day, to: readonly Day[]) {
		for (const d of to) if (d !== from) week[d] = week[from].map((p) => ({ ...p }));
		message = '';
	}

	async function save() {
		busy = true;
		message = '';
		error = '';
		const body = Object.fromEntries(
			DAYS.map((d) => [d, Object.fromEntries(sorted(week[d]).map((p) => [p.time, p.temp]))])
		);
		try {
			await post(`/devices/${device.address}`, { room_schedule: body });
			saved = JSON.stringify(week);
			message = device.room
				? `Sent to every thermostat in ${device.room}.`
				: 'Sent to this thermostat.';
			await invalidate('app:device');
		} catch (e) {
			error = (e as Error).message;
		} finally {
			busy = false;
		}
	}
</script>

<section class="card">
	<div class="row title">
		<h2>Week program</h2>
		<span class="muted">{device.room ? `for the whole ${device.room}` : 'for this thermostat'}</span>
	</div>
	{#if !known}
		<p class="muted">
			The bridge has not set a program on this device yet, so this shows the default: comfort from 6:00, eco from 22:00.
			The device keeps its own until you save.
		</p>
	{/if}

	<div class="week">
		{#each DAYS as day (day)}
			<button
				type="button"
				class="day"
				class:open={open === day}
				onclick={() => (open = open === day ? null : day)}
				aria-expanded={open === day}
			>
				<span class="name">{label[day]}</span>
				<span class="bar">
					{#each blocks(week[day]) as b (b.from)}
						<span
							class="block"
							style:left="{(b.from / 1440) * 100}%"
							style:width="{((b.to - b.from) / 1440) * 100}%"
							style:background={shade(b.temp)}
							title="{hhmm(b.from)}–{hhmm(b.to)}: {b.temp} °C"
						>
							{#if b.to - b.from >= 150}<span class="t" class:strong={strength(b.temp) >= 60}>{b.temp}°</span>{/if}
						</span>
					{/each}
				</span>
			</button>
			{#if open === day}
				<div class="edit">
					{#each sorted(week[day]) as p, k (p.time + k)}
						<div class="point">
							<input
								type="time"
								step="300"
								value={p.time}
								onchange={(e) => {
									p.time = e.currentTarget.value;
									week[day] = sorted(week[day]);
								}}
								aria-label="Start time"
							/>
							<input type="number" min="4.5" max="30.5" step="0.5" bind:value={p.temp} aria-label="Temperature" />
							<span class="muted">°C</span>
							<button
								type="button"
								class="link"
								aria-label="Remove"
								disabled={week[day].length === 1}
								onclick={() => (week[day] = week[day].filter((x) => x !== p))}>✕</button
							>
						</div>
					{/each}
					<div class="row actions">
						<button type="button" class="secondary" disabled={week[day].length >= MAX_POINTS} onclick={() => add(day)}>
							Add switch point
						</button>
						<span class="muted">Copy to</span>
						<button type="button" class="link" onclick={() => copy(day, DAYS.slice(0, 5))}>weekdays</button>
						<button type="button" class="link" onclick={() => copy(day, DAYS.slice(5))}>weekend</button>
						<button type="button" class="link" onclick={() => copy(day, DAYS)}>every day</button>
					</div>
					{#if problems(week[day])}<p class="error">{problems(week[day])}</p>{/if}
				</div>
			{/if}
		{/each}
		<div class="hours" aria-hidden="true">
			<span></span>
			<span class="scale">{#each [0, 6, 12, 18, 24] as h (h)}<span style:left="{(h / 24) * 100}%">{h}</span>{/each}</span>
		</div>
	</div>

	<p class="muted">Each time is when its temperature starts; the last one holds until the first one the next day.</p>
	{#if message}<p class="ok">{message}</p>{/if}
	{#if error}<p class="error">{error}</p>{/if}
	<div class="row">
		<button disabled={busy || !changed || invalid.length > 0} onclick={save}>{busy ? 'Sending…' : 'Save program'}</button>
		{#if changed}
			<button type="button" class="link" onclick={() => (week = JSON.parse(saved))}>Discard changes</button>
		{/if}
		{#if invalid.length}<span class="error">Fix {invalid.map(([d]) => label[d]).join(', ')}</span>{/if}
	</div>
</section>

<style>
	.title {
		justify-content: space-between;
		align-items: baseline;
	}
	.week {
		display: grid;
		gap: 0.35rem;
		margin: 0.5rem 0 0.75rem;
	}
	.day,
	.hours {
		display: grid;
		grid-template-columns: 2.5rem 1fr;
		align-items: center;
		gap: 0.6rem;
	}
	.day {
		background: none;
		color: inherit;
		padding: 0.15rem 0;
		text-align: left;
		border-radius: 6px;
	}
	.day:hover .bar,
	.day.open .bar {
		outline: 2px solid var(--accent);
		outline-offset: 1px;
	}
	.name {
		font-size: 0.85rem;
		color: var(--muted);
	}
	.open .name {
		color: var(--text);
		font-weight: 600;
	}
	.bar {
		position: relative;
		height: 1.6rem;
		border-radius: 5px;
		overflow: hidden;
		background: var(--line);
	}
	.block {
		position: absolute;
		top: 0;
		bottom: 0;
		display: flex;
		align-items: center;
		justify-content: center;
		border-right: 1px solid var(--card);
	}
	.t {
		font-size: 0.72rem;
		font-weight: 600;
		color: var(--text);
	}
	.t.strong {
		color: var(--accent-text);
	}
	.hours .scale {
		position: relative;
		height: 1rem;
		font-size: 0.7rem;
		color: var(--muted);
	}
	.scale span {
		position: absolute;
		transform: translateX(-50%);
	}
	.scale span:first-child {
		transform: none;
	}
	.scale span:last-child {
		transform: translateX(-100%);
	}
	.edit {
		margin: 0.25rem 0 0.6rem 3.1rem;
		display: grid;
		gap: 0.4rem;
	}
	.point {
		display: flex;
		gap: 0.5rem;
		align-items: center;
	}
	.point input[type='time'] {
		width: 7.5rem;
	}
	.point input[type='number'] {
		width: 5rem;
	}
	.actions {
		gap: 0.75rem;
	}
	.actions .link {
		color: var(--accent);
	}
</style>
