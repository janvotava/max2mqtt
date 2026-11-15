<script lang="ts">
	import { m } from '#lib/paraglide/messages.js';
	import { getLocale } from '#lib/paraglide/runtime.js';
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
			error = m.not_json();
			return;
		}
		if (backup.format !== 'max2mqtt-backup') {
			error = m.not_backup();
			return;
		}
		const devices = backup.config?.devices?.length ?? 0;
		const wifi = backup.settings?.wifi?.ssid;
		if (
			!confirm(
				m.restore_confirm({ n: devices, wifi: wifi ? m.restore_wifi({ ssid: wifi }) : '' })
			)
		)
			return;
		busy = true;
		try {
			await put('/backup', backup);
			await afterRestart(m.backup_restored());
		} catch (err) {
			error = (err as Error).message;
		} finally {
			busy = false;
		}
	}
	async function afterRestart(what: string) {
		message = `${what} ${m.restarting_short()}`.trim();
		message = (await waitForBoard(90)) ? m.back_up() : m.not_back_short();
		await invalidate('app:status');
	}

	async function restart(path: '/restart' | '/factory-reset') {
		if (
			path === '/factory-reset' &&
			!confirm(m.factory_confirm())
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
			await afterRestart(m.firmware_written());
		} catch (err) {
			error = (err as Error).message;
			progress = null;
		} finally {
			busy = false;
		}
	}
</script>

<h2 class="part" id="board">{m.board()}</h2>
{#if message}<p class="ok">{message}</p>{/if}
{#if error}<p class="error">{error}</p>{/if}

{#if s}
	<div class="grid">
		<div class="card">
			<h2>{m.status()}</h2>
			<dl>
				<dt>{m.firmware()}</dt><dd>max2mqtt {s.system.version}</dd>
				<dt>{m.board()}</dt><dd>{s.system.board}</dd>
				<dt>{m.up()}</dt><dd>{duration(s.system.uptime_seconds)}</dd>
				<dt>{m.memory()}</dt><dd>
					{m.memory_value({
						used: Math.round(s.system.heap_used / 1024),
						size: Math.round(s.system.heap_size / 1024),
						max: Math.round(s.system.heap_max / 1024)
					})}
				</dd>
			</dl>
		</div>
		<div class="card">
			<h2>{m.network()}</h2>
			<dl>
				<dt>Wi-Fi</dt><dd>{s.network.station ? `${s.network.ssid}, ${s.network.ip ?? m.no_address_yet()}` : m.not_connected()}</dd>
				<dt>{m.hostname()}</dt><dd>{s.network.hostname}</dd>
				<dt>{m.hotspot()}</dt><dd>{s.network.hotspot ? m.on_ssid({ ssid: s.network.hotspot_ssid }) : m.off()}</dd>
				<dt>MQTT</dt><dd>{s.network.mqtt_host ? `${s.network.mqtt_host}, ${s.network.mqtt ? m.connected() : m.not_connected()}` : m.off()}</dd>
			</dl>
		</div>
	</div>

	<form class="card" onsubmit={update}>
		<h2>{m.firmware_update()}</h2>
		<p class="muted">
			{m.running_from({ slot: s.system.firmware_slot, state: s.system.firmware_state })}
			<code>fw-image</code> (C3 Mini) {m.or()} <code>fw-esp32-image</code> (ESP32). {m.fw_rollback_hint()}
		</p>
		<div class="row">
			<input type="file" accept=".bin" onchange={(e) => (file = e.currentTarget.files?.[0] ?? null)} disabled={busy} />
			<button disabled={busy || !file}>{m.update()}</button>
		</div>
		{#if progress !== null}
			<progress max="1" value={progress}></progress>
			<p class="muted">{progress < 1 ? m.uploading({ n: Math.round(progress * 100) }) : m.writing()}</p>
		{/if}
	</form>

	<form class="card" onsubmit={restore}>
		<h2>{m.backup()}</h2>
		<p class="muted">{m.backup_hint()}</p>
		<div class="row">
			<button type="button" class="secondary" disabled={busy} onclick={download}>{m.download_backup()}</button>
		</div>
		<div class="row restore">
			<input
				type="file"
				accept=".json,application/json"
				onchange={(e) => (backupFile = e.currentTarget.files?.[0] ?? null)}
				disabled={busy}
			/>
			<button disabled={busy || !backupFile}>{m.restore()}</button>
		</div>
	</form>

{/if}

<div class="card">
	<h2>{m.crashes()}</h2>
	{#if crashes.length}
		<p class="muted">{m.crashes_hint()}</p>
		<ul class="crashes">
			{#each crashes.toReversed() as c, k (k)}
				<li>
					<div>
						<strong>{c.time ? new Date(c.time * 1000).toLocaleString(getLocale()) : m.time_unknown()}</strong>
						<span class="muted">· {c.version} · {m.after({ d: duration(c.uptime_seconds) })}</span>
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
			}}>{m.clear()}</button
		>
	{:else}
		<p class="muted">{m.no_crashes()}</p>
	{/if}
</div>

<div class="card">
	<h2>{m.restart()}</h2>
	<div class="row">
		<button class="secondary" disabled={busy} onclick={() => restart('/restart')}>{m.restart()}</button>
		<button class="danger" disabled={busy} onclick={() => restart('/factory-reset')}>{m.factory_reset()}</button>
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
