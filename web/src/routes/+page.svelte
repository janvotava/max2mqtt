<script lang="ts">
	import { m } from '#lib/paraglide/messages.js';
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
			<strong>{m.no_wifi()}</strong>
			{#if s.network.ssid}{m.trying_join({ ssid: s.network.ssid })}{/if}
			<a href="#/settings">{m.set_up_wifi()}</a>
		</div>
	{/if}
	{#if s.bridge.pairing_enabled}
		<div class="card banner"><strong>{m.pairing_on_banner()}</strong> {m.pairing_on_hint()} <a href="#/settings">{m.nav_settings()}</a></div>
	{/if}

	<div class="grid">
		<div class="card">
			<h2>{m.furnace()}</h2>
			<p class="big">{#if s.bridge.furnace_running}<span class="on">{m.on()}</span>{:else}{m.off_cap()}{/if}</p>
			<p class="muted">{m.heat_demand({ n: s.bridge.heat_demand.toFixed(0) })}</p>
		</div>
		<div class="card">
			<h2>{m.bridge()}</h2>
			<dl>
				<dt>{m.time()}</dt><dd>{s.bridge.time?.replace('T', ' ') ?? m.not_synced()}</dd>
				<dt>MQTT</dt>
				<dd class={s.network.mqtt ? 'ok' : 'muted'}>{s.network.mqtt ? m.connected() : s.network.mqtt_host ? m.not_connected() : m.off()}</dd>
				<dt>{m.radio_queue()}</dt>
				<dd>
					{s.bridge.tx_queue}{#if s.bridge.tx_waiting}<span class="muted">
							· {m.waiting_sensor({ n: s.bridge.tx_waiting })}</span
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
			<h2>{m.rooms()}</h2>
			<p class="muted">{m.no_rooms()}</p>
		</div>
	{/each}

	{#if others.length}
		<details class="card" open={!rooms.size}>
			<summary><strong>{m.other_devices()}</strong> <span class="muted">{m.other_devices_count({ n: others.length })}</span></summary>
			<p class="muted">{m.other_devices_hint()}</p>
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
