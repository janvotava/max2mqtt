<script lang="ts">
	import { m } from '#lib/paraglide/messages.js';
	import { untrack } from 'svelte';
	import { invalidate } from '$app/navigation';
	import { post } from '#lib/api.svelte.js';
	import { DAYS, type Day, type DeviceConfig } from '#lib/types.js';

	let { device }: { device: DeviceConfig } = $props();

	interface Point {
		/** Stable while the time is edited: keying rows by time rebuilt them mid-typing. */
		id: number;
		time: string;
		temp: number;
	}
	let nextId = 0;
	const point = (time: string, temp: number): Point => ({ id: nextId++, time, temp });
	type Week = Record<Day, Point[]>;

	const MAX_POINTS = 13;
	const label: Record<Day, string> = {
		monday: m.day_monday(),
		tuesday: m.day_tuesday(),
		wednesday: m.day_wednesday(),
		thursday: m.day_thursday(),
		friday: m.day_friday(),
		saturday: m.day_saturday(),
		sunday: m.day_sunday()
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
			point('06:00', d.temperatures.comfort),
			point('22:00', d.temperatures.eco)
		];
		return Object.fromEntries(
			DAYS.map((day) => {
				const stored = d.schedule?.[day];
				const points = stored ? Object.entries(stored).map(([time, temp]) => point(time, temp)) : fallback();
				return [day, sorted(points)];
			})
		) as Week;
	}

	const initial = untrack(() => $state.snapshot(device));
	const known = Object.keys(initial.schedule ?? {}).length > 0;
	let week = $state<Week>(load(initial));
	/** The program without ids or editing order, to compare and send. */
	const plain = (w: Week) =>
		Object.fromEntries(DAYS.map((d) => [d, Object.fromEntries(sorted(w[d]).map((p) => [p.time, p.temp]))]));
	let saved = $state(JSON.stringify(plain(load(initial))));
	let open = $state<Day | null>(null);
	let busy = $state(false);
	let message = $state('');
	let error = $state('');

	const changed = $derived(JSON.stringify(plain(week)) !== saved);

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
		if (!points.length) return m.wp_needs_point();
		if (points.length > MAX_POINTS) return m.wp_too_many({ n: MAX_POINTS });
		const times = points.map((p) => p.time);
		if (times.some((t) => !/^\d\d:\d\d$/.test(t) || minutes(t) % 5)) return m.wp_five_minutes();
		if (new Set(times).size !== times.length) return m.wp_same_time();
		if (points.some((p) => !(p.temp >= 4.5 && p.temp <= 30.5))) return m.wp_range();
		return null;
	}
	const invalid = $derived(DAYS.map((d) => [d, problems(week[d])] as const).filter(([, p]) => p));

	function add(day: Day) {
		const p = sorted(week[day]);
		const last = p.length ? minutes(p[p.length - 1].time) : 6 * 60;
		const time = hhmm(Math.min(23 * 60 + 55, last + 60));
		week[day] = sorted([...week[day], point(time, device.temperatures.comfort)]);
	}

	function copy(from: Day, to: readonly Day[]) {
		for (const d of to) if (d !== from) week[d] = week[from].map((p) => point(p.time, p.temp));
		message = '';
	}

	async function save() {
		busy = true;
		message = '';
		error = '';
		const body = plain(week);
		try {
			await post(`/devices/${device.address}`, { room_schedule: body });
			saved = JSON.stringify(body);
			message = device.room
				? m.wp_sent_room({ room: device.room })
				: m.wp_sent_one();
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
		<h2>{m.week_program()}</h2>
		<span class="muted">{device.room ? m.wp_for_room({ room: device.room }) : m.wp_for_one()}</span>
	</div>
	{#if !known}
		<p class="muted">{m.wp_default()}</p>
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
					{#each week[day] as p (p.id)}
						<div class="point">
							<!-- Sorted once the field is left: re-sorting while typing moved the row. -->
							<input
								type="time"
								step="300"
								bind:value={p.time}
								onblur={() => (week[day] = sorted(week[day]))}
								aria-label={m.start_time()}
							/>
							<input type="number" min="4.5" max="30.5" step="0.5" bind:value={p.temp} aria-label={m.temperature()} />
							<span class="muted">°C</span>
							<button
								type="button"
								class="link"
								aria-label={m.remove()}
								disabled={week[day].length === 1}
								onclick={() => (week[day] = week[day].filter((x) => x !== p))}>✕</button
							>
						</div>
					{/each}
					<div class="row actions">
						<button type="button" class="secondary" disabled={week[day].length >= MAX_POINTS} onclick={() => add(day)}>
							{m.add_point()}
						</button>
						<span class="muted">{m.copy_to()}</span>
						<button type="button" class="link" onclick={() => copy(day, DAYS.slice(0, 5))}>{m.weekdays()}</button>
						<button type="button" class="link" onclick={() => copy(day, DAYS.slice(5))}>{m.weekend()}</button>
						<button type="button" class="link" onclick={() => copy(day, DAYS)}>{m.every_day()}</button>
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

	<p class="muted">{m.wp_hint()}</p>
	{#if message}<p class="ok">{message}</p>{/if}
	{#if error}<p class="error">{error}</p>{/if}
	<div class="row">
		<button disabled={busy || !changed || invalid.length > 0} onclick={save}>{busy ? m.sending() : m.save_program()}</button>
		{#if changed}
			<button type="button" class="link" onclick={() => (week = load({ ...initial, schedule: JSON.parse(saved) }))}>{m.discard()}</button>
		{/if}
		{#if invalid.length}<span class="error">{m.fix_days({ days: invalid.map(([d]) => label[d]).join(', ') })}</span>{/if}
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
