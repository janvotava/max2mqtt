<script lang="ts">
	import { ago, celsius, modeName, typeName } from '#lib/format.js';
	import { m } from '#lib/paraglide/messages.js';
	import { autoRefresh } from '#lib/refresh.svelte.js';
	import DeviceForms from './DeviceForms.svelte';
	import WeekProgram from './WeekProgram.svelte';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();
	autoRefresh();
	const device = $derived(data.device);
	const live = $derived(data.status?.devices.find((d) => d.address === device.address));
	const nameOf = (address: string) => data.status?.devices.find((d) => d.address === address)?.name ?? address;
	const pendingLabel = (p: string | Record<string, string | string[]>) => {
		if (typeof p === 'string')
			return (
				({ group: m.pending_group, temperatures: m.pending_temperatures, valve: m.pending_valve, display: m.pending_display } as Record<
					string,
					() => string
				>)[p]?.() ?? p
			);
		const [kind, value] = Object.entries(p)[0];
		const target = Array.isArray(value) ? value[0] : value;
		if (kind === 'link') return m.pending_link({ name: nameOf(target) });
		if (kind === 'unlink' || kind === 'unlinked') return m.pending_unlink({ name: nameOf(target) });
		return m.pending_program({ day: String(value) });
	};
</script>

<p><a href="#/">{m.back_devices()}</a></p>

<h1>{device.name}</h1>
<p class="muted">
	{live?.type ? typeName[live.type] : m.unknown_type()} · {device.address} · {ago(live?.seen_seconds_ago)}
	{#if live?.rssi !== undefined}· {live.rssi} dBm{/if}
</p>

{#if live}
	<div class="card">
		<h2>{m.now()}</h2>
		<dl>
			<dt>{m.temperature()}</dt><dd>{celsius(live.measured_temperature)}</dd>
			<dt>{m.target()}</dt><dd>{celsius(live.desired_temperature)}</dd>
			<dt>{m.mode()}</dt><dd>{modeName(live.mode)}</dd>
			<!-- A wall thermostat's valve is just the room's most-open radiator, listed there. -->
			{#if live.valve_position !== undefined && live.type !== 'thermostat'}<dt>{m.valve()}</dt><dd>{live.valve_position} %</dd>{/if}
			{#if live.open !== undefined}<dt>{m.window()}</dt><dd>{live.open ? m.open() : m.closed()}</dd>{/if}
			<dt>{m.battery()}</dt><dd>{live.low_battery ? m.battery_low() : m.battery_ok()}</dd>
			{#if live.rf_error}<dt>{m.radio()}</dt><dd class="error">{m.no_answer_partner()}</dd>{/if}
			<dt>{m.linked_with()}</dt>
			<dd>
				{#each device.associations ?? [] as a, k (a)}{k ? ', ' : ''}<a href="#/devices/{a}">{nameOf(a)}</a>{:else}–{/each}
			</dd>
			{#if device.group}<dt>{m.group()}</dt><dd>{device.group}</dd>{/if}
			<dt>{m.settings()}</dt>
			<dd>
				{#if device.pending?.length}
					<span class="muted">{m.not_confirmed_yet()}</span>
					{device.pending.map(pendingLabel).join(', ')}
					<span class="muted">{m.sent_again_heard()}</span>
				{:else}
					{m.up_to_date()}
				{/if}
			</dd>
		</dl>
	</div>
{/if}

<!-- Fresh form state per device. -->
{#key device.address}
	<DeviceForms {device} {live} />
	{#if device.type === 'heater' || device.type === 'heater_plus' || device.type === 'thermostat'}
		<WeekProgram {device} />
	{/if}
{/key}
