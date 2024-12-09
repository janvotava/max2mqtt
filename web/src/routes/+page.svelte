<script lang="ts">
	import DeviceTable from '#lib/DeviceTable.svelte';
	import { autoRefresh } from '#lib/refresh.svelte.js';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();
	autoRefresh();
	const s = $derived(data.status);
	const rooms = $derived(Map.groupBy(s?.devices.filter((d) => d.room) ?? [], (d) => d.room!));
	const others = $derived(s?.devices.filter((d) => !d.room) ?? []);
</script>

{#if data.statusError}<p class="error">{data.statusError}</p>{/if}
{#if s}
	{#if !s.network.station}
		<div class="card banner">
			<strong>Not connected to a Wi-Fi network.</strong>
			{#if s.network.ssid}Trying to join {s.network.ssid}.{/if}
			<a href="#/settings">Set up Wi-Fi</a>
		</div>
	{/if}
	{#if s.bridge.pairing_enabled}
		<div class="card banner"><strong>Pairing is on.</strong> New devices show up below. <a href="#/settings">Settings</a></div>
	{/if}

	<div class="grid">
		<div class="card">
			<h2>Furnace</h2>
			<p class="big">{#if s.bridge.furnace_running}<span class="on">On</span>{:else}Off{/if}</p>
			<p class="muted">Heat demand {s.bridge.heat_demand.toFixed(0)} %</p>
		</div>
		<div class="card">
			<h2>Bridge</h2>
			<dl>
				<dt>Time</dt><dd>{s.bridge.time?.replace('T', ' ') ?? 'not synced'}</dd>
				<dt>MQTT</dt>
				<dd class={s.network.mqtt ? 'ok' : 'muted'}>{s.network.mqtt ? 'connected' : s.network.mqtt_host ? 'not connected' : 'off'}</dd>
				<dt>Radio queue</dt>
				<dd>
					{s.bridge.tx_queue}{#if s.bridge.tx_waiting}<span class="muted">
							· {s.bridge.tx_waiting} waiting for a window sensor to wake up</span
						>{/if}
				</dd>
			</dl>
		</div>
	</div>

	{#each rooms as [room, devices] (room)}
		<div class="card">
			<h2>{room}</h2>
			<DeviceTable {devices} />
		</div>
	{:else}
		<div class="card">
			<h2>Rooms</h2>
			<p class="muted">
				No device has a room yet. Only devices with a room control the furnace and appear in Home Assistant. Open one
				from the list below and give it a room.
			</p>
		</div>
	{/each}

	{#if others.length}
		<details class="card" open={!rooms.size}>
			<summary><strong>Other devices heard</strong> <span class="muted">({others.length}, possibly the neighbours')</span></summary>
			<p class="muted">Without a room: not part of heating, not in Home Assistant.</p>
			<DeviceTable devices={others} />
		</details>
	{/if}
{/if}

<style>
	.big {
		font-size: 1.6rem;
		margin: 0;
	}
	.on {
		color: var(--accent);
	}
	summary {
		cursor: pointer;
	}
</style>
