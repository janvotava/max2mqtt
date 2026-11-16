<script lang="ts">
	import { ago, celsius, modeName, typeName } from './format.js';
	import { m } from './paraglide/messages.js';
	import type { DeviceStatus } from './types.js';

	let {
		devices,
		showRoom = false,
		staleMinutes
	}: { devices: DeviceStatus[]; showRoom?: boolean; staleMinutes?: number } = $props();

	// Readings the burner ignores (bridge `demand`): a thermostat silent for longer than
	// `stale_minutes`. Window sensors and eco switches only send on a change, so their
	// state does not age.
	const stale = (d: DeviceStatus) =>
		staleMinutes !== undefined &&
		d.type !== 'shutter_contact' &&
		d.type !== 'push_button' &&
		d.seen_seconds_ago !== undefined &&
		d.seen_seconds_ago > staleMinutes * 60;
</script>

<div class="scroll">
	<!-- Fixed widths, so that the tables of all rooms line up under each other. -->
	<table>
		<colgroup>
			<col />
			{#if showRoom}<col style="width: 9rem" />{/if}
			<col style="width: 12rem" />
			<col style="width: 6.5rem" />
			<col style="width: 6.5rem" />
			<col style="width: 8rem" />
			<col style="width: 5.5rem" />
			<col style="width: 9rem" />
			<col style="width: 11rem" />
		</colgroup>
		<thead>
			<tr>
				<th>{m.col_name()}</th>
				{#if showRoom}<th>{m.col_room()}</th>{/if}
				<th>{m.col_type()}</th><th>{m.col_now()}</th><th>{m.col_target()}</th><th>{m.col_mode()}</th><th
					>{m.col_valve()}</th
				><th></th><th>{m.col_heard()}</th>
			</tr>
		</thead>
		<tbody>
			{#each devices as d (d.address)}
				{@const old = stale(d)}
				{@const hint = old ? m.stale_hint({ n: staleMinutes! }) : undefined}
				<tr>
					<td><a href="#/devices/{d.address}">{d.name}</a></td>
					{#if showRoom}<td class={d.room ? '' : 'muted'}>{d.room ?? '–'}</td>{/if}
					<td class="muted">{d.type ? typeName[d.type] : '?'}</td>
					<td class:stale={old} title={hint}>
						{d.type === 'shutter_contact'
							? d.open === undefined
								? '–'
								: d.open
									? m.open()
									: m.closed()
							: celsius(d.measured_temperature)}
					</td>
					<td class:stale={old} title={hint}>{celsius(d.desired_temperature)}</td>
					<td class:stale={old} title={hint}>{modeName(d.mode)}</td>
					<td class:stale={old} title={hint}
						>{d.valve_position === undefined || d.type === 'thermostat' ? '–' : `${d.valve_position} %`}</td
					>
					<td class="badges">
						{#if d.low_battery}<span class="badge on">{m.badge_battery()}</span>{/if}
						{#if d.unconfirmed}
							<span class="badge" title={m.badge_unconfirmed_hint()}>{m.badge_unconfirmed({ n: d.unconfirmed })}</span>
						{/if}
						{#if d.rf_error}
							<span class="badge on" title={m.badge_no_answer_hint()}>{m.badge_no_answer()}</span>
						{/if}
					</td>
					<td class="muted" title={hint}>
						{ago(d.seen_seconds_ago)}{#if d.rssi !== undefined}{' '}<span class="dbm">· {d.rssi} dBm</span>{/if}
					</td>
				</tr>
			{/each}
		</tbody>
	</table>
</div>

<style>
	table {
		table-layout: fixed;
		/* Below this the columns would squeeze the names; the wrapper scrolls instead. */
		min-width: 64rem;
	}
	td {
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.badges {
		white-space: normal;
	}
	.stale {
		opacity: 0.4;
	}
	/* Secondary: there to compare reception, not to read first. */
	.dbm {
		opacity: 0.6;
		font-size: 0.85em;
	}
</style>
