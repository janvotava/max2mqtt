<script lang="ts">
	import { untrack } from 'svelte';
	import { invalidate } from '$app/navigation';
	import { goto } from '$app/navigation';
	import { del, post } from '#lib/api.svelte.js';
	import { typeName } from '#lib/format.js';
	import type { DeviceConfig, DeviceStatus, DeviceType, Mode } from '#lib/types.js';

	let { device, live }: { device: DeviceConfig; live: DeviceStatus | undefined } = $props();

	// Form fields start from the stored config and what the device last reported, and are
	// not overwritten by the refreshes after that.
	const initial = untrack(() => $state.snapshot({ device, live }));
	let name = $state(initial.device.name);
	let room = $state(initial.device.room ?? '');
	let type = $state<DeviceType | ''>(initial.device.type ?? '');
	let mode = $state<Mode>(initial.live?.mode === 'auto' || initial.live?.mode === 'boost' ? initial.live.mode : 'manual');
	let temperature = $state(initial.live?.desired_temperature ?? initial.device.temperatures.comfort);
	let comfort = $state(initial.device.temperatures.comfort);
	let eco = $state(initial.device.temperatures.eco);
	let message = $state('');
	let error = $state('');

	async function forget() {
		if (!confirm(`Forget ${device.name}? It is unlinked from the others and removed from Home Assistant.`)) return;
		error = '';
		try {
			await del(`/devices/${device.address}`);
			await goto('#/', { invalidateAll: true });
		} catch (err) {
			error = (err as Error).message;
		}
	}

	const thermostat = $derived(live?.type === 'heater' || live?.type === 'heater_plus' || live?.type === 'thermostat');

	async function send(e: Event, body: Record<string, unknown>, done: string) {
		e.preventDefault();
		message = '';
		error = '';
		try {
			await post(`/devices/${device.address}`, body);
			message = done;
			await Promise.all([invalidate('app:status'), invalidate('app:device')]);
		} catch (err) {
			error = (err as Error).message;
		}
	}
</script>

{#if message}<p class="ok">{message}</p>{/if}
{#if error}<p class="error">{error}</p>{/if}

<form
	class="card"
	onsubmit={(e) => send(e, { name, room, ...(type && type !== device.type && { type }) }, 'Saved.')}
>
	<h2>Name and room</h2>
	<div class="grid">
		<label>Name (MQTT topic) <input bind:value={name} required /></label>
		<label>Room <input bind:value={room} placeholder="none: not part of heating" /></label>
		{#if !device.type}
			<!-- Only until the device sends something of its own, which tells the bridge. -->
			<label>
				Type
				<select bind:value={type}>
					<option value="">not known yet</option>
					{#each Object.entries(typeName) as [value, label] (value)}<option {value}>{label}</option>{/each}
				</select>
			</label>
		{/if}
	</div>
	{#if !device.type}
		<p class="muted">
			The bridge has only heard this device answer others, which does not say what it is. Set the type so it can be
			linked and, for a radiator thermostat, count for heating.
		</p>
	{/if}
	<p class="muted">
		Devices with a room drive the furnace, get the time from the bridge, and appear in Home Assistant. In a room the
		bridge links the thermostats and window sensors to each other: radiator thermostats then follow the wall
		thermostat and close when a window opens. Leave the room empty for devices that are not yours.
	</p>
	<div class="row">
		<button>Save</button>
		<button type="button" class="link forget" onclick={forget}>Forget device</button>
		{#if device.room}
			<button
				type="button"
				class="secondary"
				onclick={(e) =>
					send(
						e,
						{ relink: true },
						'Sending the room\'s links again. Window sensors get theirs the next time they are opened or closed.'
					)}
			>
				Re-link room
			</button>
		{/if}
	</div>
</form>

{#if thermostat}
	<form
		class="card"
		onsubmit={(e) =>
			send(e, mode === 'manual' ? { mode, temperature } : { mode }, 'Sent. The device confirms on its next message.')}
	>
		<h2>Control</h2>
		<div class="grid">
			<label>
				Mode
				<select bind:value={mode}>
					<option value="auto">Auto (week program)</option>
					<option value="manual">Manual</option>
					<option value="boost">Boost</option>
				</select>
			</label>
			{#if mode === 'manual'}
				<label>Temperature (°C) <input type="number" min="4.5" max="30.5" step="0.5" bind:value={temperature} /></label>
			{/if}
		</div>
		<button>Send</button>
	</form>

	<form
		class="card"
		onsubmit={(e) => send(e, { comfort_temperature: comfort, eco_temperature: eco }, 'Sent to the device.')}
	>
		<h2>Comfort and eco temperatures</h2>
		<div class="grid">
			<label>Comfort (°C) <input type="number" min="4.5" max="30.5" step="0.5" bind:value={comfort} /></label>
			<label>Eco (°C) <input type="number" min="4.5" max="30.5" step="0.5" bind:value={eco} /></label>
		</div>
		<button>Send</button>
	</form>
{/if}

<style>
	.forget {
		color: var(--warn);
		margin-left: auto;
		order: 1;
	}
</style>
