<script lang="ts">
	import { invalidate } from '$app/navigation';
	import { get, post, put, uploadFirmware, waitForBoard } from '#lib/api.svelte.js';
	import { duration } from '#lib/format.js';
	import type { Crash, Status } from '#lib/types.js';

	/** The board part of Settings: state, firmware, crashes, restart. */
	let { status, crashes }: { status: Status | null; crashes: Crash[] } = $props();
	const s = $derived(status);
	let message = $state('');
	let error = $state('');
	let busy = $state(false);

	let file = $state<File | null>(null);
	let progress = $state<number | null>(null);

	async function download() {
		error = '';
		try {
			const backup = await get<unknown>('/backup');
			const blob = new Blob([JSON.stringify(backup, null, 2) + '\n'], { type: 'application/json' });
			const a = document.createElement('a');
			a.href = URL.createObjectURL(blob);
			a.download = `max2mqtt-${s?.network.hostname ?? 'backup'}-${new Date().toISOString().slice(0, 10)}.json`;
			a.click();
			URL.revokeObjectURL(a.href);
		} catch (e) {
			error = (e as Error).message;
		}
	}

	let backupFile = $state<File | null>(null);
	async function restore(e: SubmitEvent) {
		e.preventDefault();
		if (!backupFile) return;
		error = '';
		message = '';
		let backup: { format?: string; settings?: { wifi?: { ssid?: string } }; config?: { devices?: unknown[] } };
		try {
			backup = JSON.parse(await backupFile.text());
		} catch {
			error = 'That file is not JSON.';
			return;
		}
		if (backup.format !== 'max2mqtt-backup') {
			error = 'That file is not a max2mqtt backup.';
			return;
		}
		const devices = backup.config?.devices?.length ?? 0;
		const wifi = backup.settings?.wifi?.ssid;
		if (
			!confirm(
				`Replace all settings and devices with the backup (${devices} devices${wifi ? `, Wi-Fi "${wifi}"` : ''})? ` +
					'The board restarts and sends every device its settings again.'
			)
		)
			return;
		busy = true;
		try {
			await put('/backup', backup);
			await afterRestart('Backup restored.');
		} catch (err) {
			error = (err as Error).message;
		} finally {
			busy = false;
		}
	}
	async function afterRestart(what: string) {
		message = `${what} Restarting…`;
		message = (await waitForBoard(90)) ? 'Back up.' : 'The board did not come back here.';
		await invalidate('app:status');
	}

	async function restart(path: '/restart' | '/factory-reset') {
		if (
			path === '/factory-reset' &&
			!confirm('Erase all settings and devices? The board restarts with its setup hotspot and the default password.')
		)
			return;
		busy = true;
		error = '';
		try {
			await post(path);
			await afterRestart('');
		} catch (e) {
			error = (e as Error).message;
		} finally {
			busy = false;
		}
	}

	async function update(e: SubmitEvent) {
		e.preventDefault();
		if (!file) return;
		busy = true;
		error = '';
		message = '';
		progress = 0;
		try {
			await uploadFirmware(file, (f) => (progress = f));
			progress = null;
			await afterRestart('Firmware written.');
		} catch (err) {
			error = (err as Error).message;
			progress = null;
		} finally {
			busy = false;
		}
	}
</script>

<h2 class="part" id="board">Board</h2>
{#if message}<p class="ok">{message}</p>{/if}
{#if error}<p class="error">{error}</p>{/if}

{#if s}
	<div class="grid">
		<div class="card">
			<h2>Status</h2>
			<dl>
				<dt>Firmware</dt><dd>max2mqtt {s.system.version}</dd>
				<dt>Board</dt><dd>{s.system.board}</dd>
				<dt>Up</dt><dd>{duration(s.system.uptime_seconds)}</dd>
				<dt>Memory</dt><dd>
					{Math.round(s.system.heap_used / 1024)} of {Math.round(s.system.heap_size / 1024)} KiB, at most {Math.round(
						s.system.heap_max / 1024
					)}
				</dd>
			</dl>
		</div>
		<div class="card">
			<h2>Network</h2>
			<dl>
				<dt>Wi-Fi</dt><dd>{s.network.station ? `${s.network.ssid}, ${s.network.ip ?? 'no address yet'}` : 'not connected'}</dd>
				<dt>Hostname</dt><dd>{s.network.hostname}</dd>
				<dt>Hotspot</dt><dd>{s.network.hotspot ? `on (${s.network.hotspot_ssid})` : 'off'}</dd>
				<dt>MQTT</dt><dd>{s.network.mqtt_host ? `${s.network.mqtt_host}, ${s.network.mqtt ? 'connected' : 'not connected'}` : 'off'}</dd>
			</dl>
		</div>
	</div>

	<form class="card" onsubmit={update}>
		<h2>Firmware update</h2>
		<p class="muted">
			Running from {s.system.firmware_slot} ({s.system.firmware_state}). Build the image with
			<code>fw-image</code> (C3 Mini) or <code>fw-esp32-image</code> (ESP32). A new image goes back to the previous one
			by itself if it keeps crashing or cannot get onto the network within 10 minutes.
		</p>
		<div class="row">
			<input type="file" accept=".bin" onchange={(e) => (file = e.currentTarget.files?.[0] ?? null)} disabled={busy} />
			<button disabled={busy || !file}>Update</button>
		</div>
		{#if progress !== null}
			<progress max="1" value={progress}></progress>
			<p class="muted">{progress < 1 ? `Uploading ${Math.round(progress * 100)} %` : 'Writing and checking…'}</p>
		{/if}
	</form>

	<form class="card" onsubmit={restore}>
		<h2>Backup</h2>
		<p class="muted">
			One file with all settings and devices: rooms, temperatures, week programs and the radio address they are paired
			to. It contains the Wi-Fi and MQTT passwords, so keep it private. The device password is not in it: a restore keeps the
			current one.
		</p>
		<div class="row">
			<button type="button" class="secondary" disabled={busy} onclick={download}>Download backup</button>
		</div>
		<div class="row restore">
			<input
				type="file"
				accept=".json,application/json"
				onchange={(e) => (backupFile = e.currentTarget.files?.[0] ?? null)}
				disabled={busy}
			/>
			<button disabled={busy || !backupFile}>Restore</button>
		</div>
	</form>

{/if}

<div class="card">
	<h2>Crashes</h2>
	{#if crashes.length}
		<p class="muted">
			The board restarted after these. Turn the addresses into source lines with <code>fw-crash</code> and the ELF of
			that version.
		</p>
		<ul class="crashes">
			{#each crashes.toReversed() as c, k (k)}
				<li>
					<div>
						<strong>{c.time ? new Date(c.time * 1000).toLocaleString() : 'time unknown'}</strong>
						<span class="muted">· {c.version} · after {duration(c.uptime_seconds)}</span>
					</div>
					<div class="error">{c.reason}</div>
					{#if c.backtrace.length}<code>{c.backtrace.join(' ')}</code>{/if}
				</li>
			{/each}
		</ul>
		<button
			class="secondary"
			onclick={async () => {
				await post('/crashes/clear');
				await invalidate('app:crashes');
			}}>Clear</button
		>
	{:else}
		<p class="muted">None recorded. A crash restarts the board and shows up here.</p>
	{/if}
</div>

<div class="card">
	<h2>Restart</h2>
	<div class="row">
		<button class="secondary" disabled={busy} onclick={() => restart('/restart')}>Restart</button>
		<button class="danger" disabled={busy} onclick={() => restart('/factory-reset')}>Factory reset</button>
	</div>
</div>

<style>
	.crashes {
		list-style: none;
		padding: 0;
		margin: 0 0 0.75rem;
		display: grid;
		gap: 0.75rem;
	}
	.crashes code {
		font-size: 0.8rem;
		word-break: break-all;
	}
	.restore {
		margin-top: 0.75rem;
	}
	progress {
		width: 100%;
		margin-top: 0.75rem;
		accent-color: var(--accent);
	}
</style>
