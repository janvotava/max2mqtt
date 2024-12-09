<script lang="ts">
	import { untrack } from 'svelte';
	import { invalidate } from '$app/navigation';
	import { get, post } from '#lib/api.svelte.js';
	import { restart, saveAndRestart } from '#lib/restart.svelte.js';
	import type { Network, Settings } from '#lib/types.js';
	import { autoRefresh } from '#lib/refresh.svelte.js';
	import Board from './Board.svelte';
	import FrequencyCheck from './FrequencyCheck.svelte';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();
	// Live values; every 2 s only while pairing counts down. The forms keep their own copies.
	autoRefresh(() => (data.status?.bridge.pairing_enabled ? 2 : 10));

	// Forms edit copies, taken once: the status refreshes in the background.
	const start = untrack(() => $state.snapshot({ settings: data.settings, bridge: data.status?.bridge }));
	let s = $state<Settings>(start.settings);
	let address = $state(start.bridge?.address ?? '');
	let tzOffset = $state(start.bridge?.timezone.utc_offset_minutes ?? 60);
	let tzDst = $state(start.bridge?.timezone.eu_dst ?? true);
	let heating = $state(
		start.bridge?.heating ?? { cycle_minutes: 20, min_on_minutes: 3, min_off_minutes: 5, min_valve_position: 30, stale_minutes: 10 }
	);
	// Empty: keep the stored password.
	let wifiPassword = $state('');
	let mqttPassword = $state('');
	let devicePassword = $state('');

	const saved = $derived(data.settings);
	const bridge = $derived(data.status?.bridge);

	/** Which section is saving, and its outcome. */
	let busy = $state('');
	let notes = $state<Record<string, { ok?: string; error?: string }>>({});

	async function run(section: string, work: () => Promise<string | void>) {
		busy = section;
		notes[section] = {};
		try {
			const ok = await work();
			if (ok) notes[section] = { ok };
		} catch (e) {
			notes[section] = { error: (e as Error).message };
		} finally {
			busy = '';
		}
	}
	const restartWith = (section: string, patch: Record<string, unknown>) =>
		run(section, () => saveAndRestart(patch, (patch.hostname as string) ?? s.hostname));
	const live = (section: string, body: Record<string, unknown>, ok: string) =>
		run(section, async () => {
			await post('/bridge', body);
			await invalidate('app:status');
			return ok;
		});

	// ----- Wi-Fi
	let networks = $state<Network[] | null>(null);
	let scanning = $state(false);
	async function scan() {
		scanning = true;
		try {
			networks = await get<Network[]>('/networks', 25000);
		} catch (e) {
			notes.wifi = { error: (e as Error).message };
		} finally {
			scanning = false;
		}
	}

	// ----- Radio and time: some fields apply live, some after a restart.
	const radioRestart = $derived(s.cc1101_freq_offset !== saved.cc1101_freq_offset);
	const radioLive = $derived(!!bridge && address.toUpperCase() !== bridge.address);
	function saveRadio(e: SubmitEvent) {
		e.preventDefault();
		run('radio', async () => {
			if (radioLive) {
				await post('/bridge', { address: address.toUpperCase() });
				await invalidate('app:status');
			}
			if (radioRestart) await saveAndRestart({ cc1101_freq_offset: s.cc1101_freq_offset }, s.hostname);
			else return 'Saved.';
		});
	}

	const OFFSETS = [...Array.from({ length: 27 }, (_, k) => (k - 12) * 60), -570, -210, 210, 270, 330, 345, 390, 525, 570, 630, 765].sort(
		(a, b) => a - b
	);
	const ZONES: Record<number, string> = { 0: 'UK, Portugal', 60: 'Central Europe', 120: 'Eastern Europe', 180: 'Moscow, Istanbul' };
	const utc = (m: number) => {
		const a = Math.abs(m);
		return `UTC${m < 0 ? '−' : '+'}${String(Math.floor(a / 60)).padStart(2, '0')}:${String(a % 60).padStart(2, '0')}${ZONES[m] ? ` (${ZONES[m]})` : ''}`;
	};
	const timeRestart = $derived(s.ntp_server !== saved.ntp_server);
	const timeLive = $derived(!!bridge && (tzOffset !== bridge.timezone.utc_offset_minutes || tzDst !== bridge.timezone.eu_dst));
	function saveTime(e: SubmitEvent) {
		e.preventDefault();
		run('time', async () => {
			if (timeLive) {
				await post('/bridge', { timezone: { utc_offset_minutes: tzOffset, eu_dst: tzDst } });
				await invalidate('app:status');
			}
			if (timeRestart) await saveAndRestart({ ntp_server: s.ntp_server }, s.hostname);
			else return 'Saved and sent to the thermostats.';
		});
	}

	const label = (restarts: boolean) => (restarts ? 'Save and restart' : 'Save');

	const heatingChanged = $derived(!!bridge && JSON.stringify(heating) !== JSON.stringify(bridge.heating));
	const share = (valve: number) =>
		valve < heating.min_valve_position ? 0 : Math.round((valve / 100) * heating.cycle_minutes * 10) / 10;
</script>

{#snippet note(section: string)}
	{#if notes[section]?.ok}<p class="ok">{notes[section].ok}</p>{/if}
	{#if notes[section]?.error}<p class="error">{notes[section].error}</p>{/if}
{/snippet}

<h1>Settings</h1>

<nav class="sections" aria-label="Sections">
	{#each [['wifi', 'Wi-Fi'], ['mqtt', 'MQTT'], ['pairing', 'Pairing'], ['radio', 'Radio'], ['heating', 'Heating'], ['time', 'Time'], ['access', 'Access'], ['board', 'Board']] as [id, name] (id)}
		<button type="button" class="link" onclick={() => document.getElementById(id)?.scrollIntoView({ behavior: 'smooth' })}>{name}</button>
	{/each}
</nav>

{#if restart.phase === 'waiting'}
	<div class="card banner">
		<p><strong>Saved. The board is restarting…</strong></p>
		<p class="muted">
			After a Wi-Fi change, connect this device to that network too: the board will be at
			<a href="http://{restart.hostname}/">http://{restart.hostname}/</a> (or <code>{restart.hostname}.lan</code>).
		</p>
	</div>
{:else if restart.phase === 'gone'}
	<div class="card banner">
		<p><strong>The board did not come back here.</strong></p>
		<p class="muted">
			It probably joined another network: look for it at <a href="http://{restart.hostname}/">http://{restart.hostname}/</a>.
			If it cannot join, its setup hotspot comes back within about a minute.
		</p>
	</div>
{:else}
	{#if restart.phase === 'back'}<p class="ok">Saved and restarted.</p>{/if}

	<form
		class="card"
		onsubmit={(e) => {
			e.preventDefault();
			restartWith('wifi', { wifi: { ssid: s.wifi.ssid, ...(wifiPassword && { password: wifiPassword }) } });
		}}
	>
		<h2 id="wifi">Wi-Fi</h2>
		<div class="grid">
			<label>
				Network
				<input bind:value={s.wifi.ssid} list="networks" placeholder="none: setup hotspot only" maxlength="32" />
				<datalist id="networks">{#each networks ?? [] as n (n.ssid)}<option value={n.ssid}>{n.rssi} dBm</option>{/each}</datalist>
			</label>
			<label>
				Password
				<input
					type="password"
					bind:value={wifiPassword}
					autocomplete="new-password"
					placeholder={s.wifi.password_set ? 'unchanged' : 'none (open network)'}
				/>
			</label>
		</div>
		{#if networks}
			<ul class="networks">
				{#each networks as n (n.ssid)}
					<li>
						<button type="button" class="link" onclick={() => (s.wifi.ssid = n.ssid)}>{n.ssid}</button>
						<span class="muted">{n.rssi} dBm{n.secure ? '' : ', open'}</span>
					</li>
				{:else}
					<li class="muted">No networks found.</li>
				{/each}
			</ul>
		{/if}
		{@render note('wifi')}
		<div class="row">
			<button disabled={busy !== ''}>Save and restart</button>
			<button type="button" class="secondary" onclick={scan} disabled={scanning}>{scanning ? 'Scanning…' : 'Scan for networks'}</button>
		</div>
	</form>

	<form
		class="card"
		onsubmit={(e) => {
			e.preventDefault();
			const { password_set: _, ...mqtt } = s.mqtt;
			restartWith('mqtt', { mqtt: { ...mqtt, ...(mqttPassword && { password: mqttPassword }) } });
		}}
	>
		<h2 id="mqtt">MQTT and Home Assistant</h2>
		<div class="grid">
			<label>Broker <input bind:value={s.mqtt.host} placeholder="none: MQTT off" /></label>
			<label>Port <input type="number" min="1" max="65535" bind:value={s.mqtt.port} /></label>
			<label>User <input bind:value={s.mqtt.user} autocomplete="off" placeholder="none" /></label>
			<label>
				Password
				<input type="password" bind:value={mqttPassword} autocomplete="new-password" placeholder={s.mqtt.password_set ? 'unchanged' : 'none'} />
			</label>
			<label>Topic prefix <input bind:value={s.mqtt.prefix} required /></label>
		</div>
		<label class="check"><input type="checkbox" bind:checked={s.mqtt.homeassistant} /> Announce devices to Home Assistant</label>
		{#if s.mqtt.homeassistant}
			<div class="grid"><label>Discovery prefix <input bind:value={s.mqtt.discovery_prefix} required /></label></div>
			<p class="muted">Devices with a room appear as thermostats, window sensors and so on; the bridge adds the furnace and a pairing switch.</p>
		{/if}
		{@render note('mqtt')}
		<button disabled={busy !== ''}>Save and restart</button>
	</form>

	{#if bridge}
		{@const left = bridge.pairing_seconds_left ?? 0}
		<div class="card">
			<h2 id="pairing">Pairing</h2>
			{#if bridge.pairing_enabled}
				<p>
					<strong>Pairing is on</strong> for another {Math.floor(left / 60)}:{String(left % 60).padStart(2, '0')}. Put the
					device into pairing mode (on most, hold the boost button until the display counts down); it then shows up under
					Devices, where you give it a name and a room.
				</p>
				{@render note('pair')}
				<button class="secondary" onclick={() => live('pair', { pair: false }, '')}>Stop pairing</button>
			{:else}
				<p class="muted">
					For new devices only: those paired to this bridge's radio address already work. Pairing stops by itself after 5
					minutes.
				</p>
				{@render note('pair')}
				<button onclick={() => live('pair', { pair: true }, '')}>Start pairing</button>
			{/if}
		</div>
	{/if}

	<form class="card" onsubmit={saveRadio}>
		<h2 id="radio">Radio</h2>
		<div class="grid">
			<label>
				Radio address
				<input bind:value={address} required pattern="[0-9A-Fa-f]{6}" maxlength="6" />
			</label>
			<label>
				Frequency correction (steps of 1.587 kHz)
				<input type="number" min="-128" max="127" step="1" bind:value={s.cc1101_freq_offset} />
			</label>
		</div>
		<p class="muted">
			Devices only listen to the address they were paired to: to take over from a MAX! Cube or another bridge, use its
			address. The frequency correction makes up for this CC1101 module's crystal error; measure it from the
			thermostats' packets:
		</p>
		<FrequencyCheck value={s.cc1101_freq_offset} onuse={(n) => (s.cc1101_freq_offset = n)} />
		{#if bridge}
			<label class="check">
				<input type="checkbox" checked={bridge.duty_cycle_limit} onchange={(e) => live('radio', { duty_cycle_limit: e.currentTarget.checked }, 'Saved.')} />
				Keep to the 1 % duty cycle of the 868 MHz band (airtime left {(bridge.credit_ms / 1000).toFixed(1)} s)
			</label>
		{/if}
		{@render note('radio')}
		<button disabled={busy !== '' || !(radioLive || radioRestart)}>{label(radioRestart)}</button>
	</form>

	<form
		class="card"
		onsubmit={(e) => {
			e.preventDefault();
			live('heating', { heating: $state.snapshot(heating) }, 'Saved.');
		}}
	>
		<h2 id="heating">Heating</h2>
		<p class="muted">
			The burner follows the most-open valve: at {heating.cycle_minutes} minute cycles, a valve 60 % open runs it
			{share(60)} minutes per cycle, one at 100 % all the time. Boost runs it continuously.
		</p>
		<div class="grid">
			<label>Cycle (minutes) <input type="number" min="5" max="60" step="1" bind:value={heating.cycle_minutes} /></label>
			<label>Valves count from (%) <input type="number" min="0" max="100" step="5" bind:value={heating.min_valve_position} /></label>
			<label>Minimum on (minutes) <input type="number" min="0" max="30" step="0.5" bind:value={heating.min_on_minutes} /></label>
			<label>Minimum off (minutes) <input type="number" min="0" max="30" step="0.5" bind:value={heating.min_off_minutes} /></label>
			<label>Ignore valves silent for (minutes) <input type="number" min="3" max="120" step="1" bind:value={heating.stale_minutes} /></label>
		</div>
		{@render note('heating')}
		<button disabled={busy !== '' || !heatingChanged}>Save</button>
	</form>

	<form class="card" onsubmit={saveTime}>
		<h2 id="time">Time</h2>
		<p class="muted">
			Local time here: <strong>{bridge?.time?.replace('T', ' ') ?? 'not synced yet'}</strong>. Thermostats run their
			week programs on the time the bridge sends them.
		</p>
		<div class="grid">
			<label>
				Standard time (winter)
				<select bind:value={tzOffset}>{#each OFFSETS as m (m)}<option value={m}>{utc(m)}</option>{/each}</select>
			</label>
			<label>Time server <input bind:value={s.ntp_server} required /></label>
		</div>
		<label class="check"><input type="checkbox" bind:checked={tzDst} /> EU summer time (last Sunday of March to last Sunday of October)</label>
		{@render note('time')}
		<button disabled={busy !== '' || !(timeLive || timeRestart)}>{label(timeRestart)}</button>
	</form>

	<form
		class="card"
		onsubmit={(e) => {
			e.preventDefault();
			restartWith('access', { hostname: s.hostname, ...(devicePassword && { password: devicePassword }) });
		}}
	>
		<h2 id="access">Access</h2>
		<div class="grid">
			<label>
				Hostname
				<input bind:value={s.hostname} required pattern="[A-Za-z0-9]([A-Za-z0-9\-]*[A-Za-z0-9])?" maxlength="32" />
			</label>
			<label>
				Password (web UI and setup hotspot)
				<input type="password" bind:value={devicePassword} autocomplete="new-password" minlength="8" maxlength="63" placeholder="unchanged" />
			</label>
		</div>
		{@render note('access')}
		<button disabled={busy !== ''}>Save and restart</button>
	</form>

	<Board status={data.status} crashes={data.crashes} />
{/if}

<style>
	.sections {
		display: flex;
		flex-wrap: wrap;
		gap: 0.25rem 1rem;
		margin: -0.25rem 0 1rem;
	}
	.sections .link {
		color: var(--accent);
	}
	:global(h2.part) {
		font-size: 1.2rem;
		margin: 2rem 0 0.75rem;
	}
	.networks {
		list-style: none;
		padding: 0;
		margin: 0 0 0.75rem;
		display: grid;
		gap: 0.3rem;
	}
	.networks button {
		color: var(--text);
		text-decoration: underline;
		margin-right: 0.5rem;
	}
	form .row {
		margin-top: 0.25rem;
	}
</style>
