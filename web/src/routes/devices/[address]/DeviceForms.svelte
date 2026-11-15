<script lang="ts">
	import { m } from '#lib/paraglide/messages.js';
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
		if (!confirm(m.forget_confirm({ name: device.name }))) return;
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
	onsubmit={(e) => send(e, { name, room, ...(type && type !== device.type && { type }) }, m.saved())}
>
	<h2>{m.name_and_room()}</h2>
	<div class="grid">
		<label>{m.name_mqtt()} <input bind:value={name} required /></label>
		<label>{m.room()} <input bind:value={room} placeholder={m.room_none()} /></label>
		{#if !device.type}
			<!-- Only until the device sends something of its own, which tells the bridge. -->
			<label>
				{m.type()}
				<select bind:value={type}>
					<option value="">{m.not_known_yet()}</option>
					{#each Object.entries(typeName) as [value, label] (value)}<option {value}>{label}</option>{/each}
				</select>
			</label>
		{/if}
	</div>
	{#if !device.type}
		<p class="muted">{m.type_unknown_hint()}</p>
	{/if}
	<p class="muted">{m.room_hint()}</p>
	<div class="row">
		<button>{m.save()}</button>
		<button type="button" class="link forget" onclick={forget}>{m.forget_device()}</button>
		{#if device.room}
			<button
				type="button"
				class="secondary"
				onclick={(e) => send(e, { relink: true }, m.relink_sent())}
			>
				{m.relink_room()}
			</button>
		{/if}
	</div>
</form>

{#if thermostat}
	<form
		class="card"
		onsubmit={(e) =>
			send(e, mode === 'manual' ? { mode, temperature } : { mode }, m.control_sent())}
	>
		<h2>{m.control()}</h2>
		<div class="grid">
			<label>
				{m.mode()}
				<select bind:value={mode}>
					<option value="auto">{m.mode_auto_option()}</option>
					<option value="manual">{m.mode_manual_option()}</option>
					<option value="boost">{m.mode_boost_option()}</option>
				</select>
			</label>
			{#if mode === 'manual'}
				<label>{m.temperature_c()} <input type="number" min="4.5" max="30.5" step="0.5" bind:value={temperature} /></label>
			{/if}
		</div>
		<button>{m.send()}</button>
	</form>

	<form
		class="card"
		onsubmit={(e) => send(e, { comfort_temperature: comfort, eco_temperature: eco }, m.sent_to_device())}
	>
		<h2>{m.comfort_eco()}</h2>
		<div class="grid">
			<label>{m.comfort_c()} <input type="number" min="4.5" max="30.5" step="0.5" bind:value={comfort} /></label>
			<label>{m.eco_c()} <input type="number" min="4.5" max="30.5" step="0.5" bind:value={eco} /></label>
		</div>
		<button>{m.send()}</button>
	</form>
{/if}

<style>
	.forget {
		color: var(--warn);
		margin-left: auto;
		order: 1;
	}
</style>
