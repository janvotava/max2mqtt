<script lang="ts">
	import { ago, celsius, typeName } from '#lib/format.js';
	import { autoRefresh } from '#lib/refresh.svelte.js';
	import DeviceForms from './DeviceForms.svelte';
	import WeekProgram from './WeekProgram.svelte';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();
	autoRefresh();
	const device = $derived(data.device);
	const live = $derived(data.status?.devices.find((d) => d.address === device.address));
	const nameOf = (address: string) => data.status?.devices.find((d) => d.address === address)?.name ?? address;
	const pendingLabel = (p: string | Record<string, string>) => {
		if (typeof p === 'string') return { group: 'group id', temperatures: 'temperatures', valve: 'valve settings', display: 'display' }[p] ?? p;
		const [kind, value] = Object.entries(p)[0];
		return kind === 'link' ? `link to ${nameOf(value)}` : kind === 'unlink' ? `unlink ${nameOf(value)}` : `${value} program`;
	};
</script>

<p><a href="#/">← Devices</a></p>

<h1>{device.name}</h1>
<p class="muted">
	{live?.type ? typeName[live.type] : 'Unknown type'} · {device.address} · {ago(live?.seen_seconds_ago)}
	{#if live?.rssi !== undefined}· {live.rssi} dBm{/if}
</p>

{#if live}
	<div class="card">
		<h2>Now</h2>
		<dl>
			<dt>Temperature</dt><dd>{celsius(live.measured_temperature)}</dd>
			<dt>Target</dt><dd>{celsius(live.desired_temperature)}</dd>
			<dt>Mode</dt><dd>{live.mode ?? '–'}</dd>
			<!-- A wall thermostat's valve is just the room's most-open radiator, listed there. -->
			{#if live.valve_position !== undefined && live.type !== 'thermostat'}<dt>Valve</dt><dd>{live.valve_position} %</dd>{/if}
			{#if live.open !== undefined}<dt>Window</dt><dd>{live.open ? 'open' : 'closed'}</dd>{/if}
			<dt>Battery</dt><dd>{live.low_battery ? 'low' : 'ok'}</dd>
			{#if live.rf_error}<dt>Radio</dt><dd class="error">got no answer from a link partner</dd>{/if}
			<dt>Linked with</dt>
			<dd>
				{#each device.associations ?? [] as a, k (a)}{k ? ', ' : ''}<a href="#/devices/{a}">{nameOf(a)}</a>{:else}–{/each}
			</dd>
			{#if device.group}<dt>Group</dt><dd>{device.group}</dd>{/if}
			<dt>Settings</dt>
			<dd>
				{#if device.pending?.length}
					<span class="muted">not confirmed yet:</span>
					{device.pending.map(pendingLabel).join(', ')}
					<span class="muted">(sent again when the device is heard)</span>
				{:else}
					up to date
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
