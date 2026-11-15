<script lang="ts">
	import { m } from '#lib/paraglide/messages.js';
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
	const ZONES: Record<number, string> = { 0: m.zone_uk(), 60: m.zone_central(), 120: m.zone_eastern(), 180: m.zone_moscow() };
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
			else return m.time_saved();
		});
	}

	const label = (restarts: boolean) => (restarts ? m.save_restart() : m.save());

	const heatingChanged = $derived(!!bridge && JSON.stringify(heating) !== JSON.stringify(bridge.heating));
	const share = (valve: number) =>
		valve < heating.min_valve_position ? 0 : Math.round((valve / 100) * heating.cycle_minutes * 10) / 10;
</script>

{#snippet note(section: string)}
	{#if notes[section]?.ok}<p class="ok">{notes[section].ok}</p>{/if}
	{#if notes[section]?.error}<p class="error">{notes[section].error}</p>{/if}
{/snippet}

<h1>{m.nav_settings()}</h1>

<nav class="sections" aria-label={m.sections()}>
	{#each [['wifi', m.sec_wifi()], ['mqtt', m.sec_mqtt()], ['pairing', m.sec_pairing()], ['radio', m.sec_radio()], ['heating', m.sec_heating()], ['time', m.sec_time()], ['access', m.sec_access()], ['board', m.sec_board()]] as [id, name] (id)}
		<button type="button" class="link" onclick={() => document.getElementById(id)?.scrollIntoView({ behavior: 'smooth' })}>{name}</button>
	{/each}
</nav>

{#if restart.phase === 'waiting'}
	<div class="card banner">
		<p><strong>{m.restarting()}</strong></p>
		<p class="muted">
			{m.restarting_hint()}
			<a href="http://{restart.hostname}/">http://{restart.hostname}/</a> ({m.or()} <code>{restart.hostname}.lan</code>).
		</p>
	</div>
{:else if restart.phase === 'gone'}
	<div class="card banner">
		<p><strong>{m.not_back()}</strong></p>
		<p class="muted">
			{m.not_back_hint()} <a href="http://{restart.hostname}/">http://{restart.hostname}/</a>.
			{m.not_back_hotspot()}
		</p>
	</div>
{:else}
	{#if restart.phase === 'back'}<p class="ok">{m.saved_restarted()}</p>{/if}

	<form
		class="card"
		onsubmit={(e) => {
			e.preventDefault();
			restartWith('wifi', { wifi: { ssid: s.wifi.ssid, ...(wifiPassword && { password: wifiPassword }) } });
		}}
	>
		<h2 id="wifi">{m.sec_wifi()}</h2>
		<div class="grid">
			<label>
				{m.network()}
				<input bind:value={s.wifi.ssid} list="networks" placeholder={m.network_none()} maxlength="32" />
				<datalist id="networks">{#each networks ?? [] as n (n.ssid)}<option value={n.ssid}>{n.rssi} dBm</option>{/each}</datalist>
			</label>
			<label>
				{m.password()}
				<input
					type="password"
					bind:value={wifiPassword}
					autocomplete="new-password"
					placeholder={s.wifi.password_set ? m.unchanged() : m.open_network()}
				/>
			</label>
		</div>
		{#if networks}
			<ul class="networks">
				{#each networks as n (n.ssid)}
					<li>
						<button type="button" class="link" onclick={() => (s.wifi.ssid = n.ssid)}>{n.ssid}</button>
						<span class="muted">{n.rssi} dBm{n.secure ? '' : m.net_open()}</span>
					</li>
				{:else}
					<li class="muted">{m.no_networks()}</li>
				{/each}
			</ul>
		{/if}
		{@render note('wifi')}
		<div class="row">
			<button disabled={busy !== ''}>{m.save_restart()}</button>
			<button type="button" class="secondary" onclick={scan} disabled={scanning}>{scanning ? m.scanning() : m.scan()}</button>
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
		<h2 id="mqtt">{m.mqtt_ha()}</h2>
		<div class="grid">
			<label>{m.broker()} <input bind:value={s.mqtt.host} placeholder={m.broker_none()} /></label>
			<label>{m.port()} <input type="number" min="1" max="65535" bind:value={s.mqtt.port} /></label>
			<label>{m.user()} <input bind:value={s.mqtt.user} autocomplete="off" placeholder={m.none()} /></label>
			<label>
				{m.password()}
				<input type="password" bind:value={mqttPassword} autocomplete="new-password" placeholder={s.mqtt.password_set ? m.unchanged() : m.none()} />
			</label>
			<label>{m.topic_prefix()} <input bind:value={s.mqtt.prefix} required /></label>
		</div>
		<label class="check"><input type="checkbox" bind:checked={s.mqtt.homeassistant} /> {m.announce_ha()}</label>
		{#if s.mqtt.homeassistant}
			<div class="grid"><label>{m.discovery_prefix()} <input bind:value={s.mqtt.discovery_prefix} required /></label></div>
			<p class="muted">{m.ha_hint()}</p>
		{/if}
		{@render note('mqtt')}
		<button disabled={busy !== ''}>{m.save_restart()}</button>
	</form>

	{#if bridge}
		{@const left = bridge.pairing_seconds_left ?? 0}
		<div class="card">
			<h2 id="pairing">{m.sec_pairing()}</h2>
			{#if bridge.pairing_enabled}
				<p>
					<strong>{m.pairing_on_for()}</strong>
					{m.pairing_for_left({ left: `${Math.floor(left / 60)}:${String(left % 60).padStart(2, '0')}` })}
					{m.pairing_how()}
				</p>
				{@render note('pair')}
				<button class="secondary" onclick={() => live('pair', { pair: false }, '')}>{m.stop_pairing()}</button>
			{:else}
				<p class="muted">{m.pairing_hint()}</p>
				{@render note('pair')}
				<button onclick={() => live('pair', { pair: true }, '')}>{m.start_pairing()}</button>
			{/if}
		</div>
	{/if}

	<form class="card" onsubmit={saveRadio}>
		<h2 id="radio">{m.sec_radio()}</h2>
		<div class="grid">
			<label>
				{m.radio_address()}
				<input bind:value={address} required pattern="[0-9A-Fa-f]{6}" maxlength="6" />
			</label>
			<label>
				{m.freq_correction()}
				<input type="number" min="-128" max="127" step="1" bind:value={s.cc1101_freq_offset} />
			</label>
		</div>
		<p class="muted">{m.radio_hint()}</p>
		<FrequencyCheck value={s.cc1101_freq_offset} onuse={(n) => (s.cc1101_freq_offset = n)} />
		{#if bridge}
			<label class="check">
				<input type="checkbox" checked={bridge.duty_cycle_limit} onchange={(e) => live('radio', { duty_cycle_limit: e.currentTarget.checked }, m.saved())} />
				{m.duty_cycle({ s: (bridge.credit_ms / 1000).toFixed(1) })}
			</label>
		{/if}
		{@render note('radio')}
		<button disabled={busy !== '' || !(radioLive || radioRestart)}>{label(radioRestart)}</button>
	</form>

	<form
		class="card"
		onsubmit={(e) => {
			e.preventDefault();
			live('heating', { heating: $state.snapshot(heating) }, m.saved());
		}}
	>
		<h2 id="heating">{m.heating()}</h2>
		<p class="muted">{m.heating_hint({ cycle: heating.cycle_minutes, share: share(60) })}</p>
		<div class="grid">
			<label>{m.cycle_min()} <input type="number" min="5" max="60" step="1" bind:value={heating.cycle_minutes} /></label>
			<label>{m.valves_from()} <input type="number" min="0" max="100" step="5" bind:value={heating.min_valve_position} /></label>
			<label>{m.min_on()} <input type="number" min="0" max="30" step="0.5" bind:value={heating.min_on_minutes} /></label>
			<label>{m.min_off()} <input type="number" min="0" max="30" step="0.5" bind:value={heating.min_off_minutes} /></label>
			<label>{m.stale()} <input type="number" min="3" max="120" step="1" bind:value={heating.stale_minutes} /></label>
		</div>
		{@render note('heating')}
		<button disabled={busy !== '' || !heatingChanged}>{m.save()}</button>
	</form>

	<form class="card" onsubmit={saveTime}>
		<h2 id="time">{m.sec_time()}</h2>
		<p class="muted">
			{m.local_time()} <strong>{bridge?.time?.replace('T', ' ') ?? m.not_synced_yet()}</strong>. {m.time_hint()}
		</p>
		<div class="grid">
			<label>
				{m.standard_time()}
				<select bind:value={tzOffset}>{#each OFFSETS as m (m)}<option value={m}>{utc(m)}</option>{/each}</select>
			</label>
			<label>{m.time_server()} <input bind:value={s.ntp_server} required /></label>
		</div>
		<label class="check"><input type="checkbox" bind:checked={tzDst} /> {m.eu_dst()}</label>
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
		<h2 id="access">{m.sec_access()}</h2>
		<div class="grid">
			<label>
				{m.hostname()}
				<input bind:value={s.hostname} required pattern="[A-Za-z0-9]([A-Za-z0-9\-]*[A-Za-z0-9])?" maxlength="32" />
			</label>
			<label>
				{m.device_password()}
				<input type="password" bind:value={devicePassword} autocomplete="new-password" minlength="8" maxlength="63" placeholder={m.unchanged()} />
			</label>
		</div>
		{@render note('access')}
		<button disabled={busy !== ''}>{m.save_restart()}</button>
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
