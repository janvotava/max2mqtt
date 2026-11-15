<script lang="ts">
	import { ago, celsius, modeName, typeName } from './format.js';
	import { m } from './paraglide/messages.js';
	import type { DeviceStatus } from './types.js';

	let { devices, showRoom = false }: { devices: DeviceStatus[]; showRoom?: boolean } = $props();
</script>

<div class="scroll">
	<table>
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
				<tr>
					<td><a href="#/devices/{d.address}">{d.name}</a></td>
					{#if showRoom}<td class={d.room ? '' : 'muted'}>{d.room ?? '–'}</td>{/if}
					<td class="muted">{d.type ? typeName[d.type] : '?'}</td>
					<td>
						{d.type === 'shutter_contact'
							? d.open === undefined
								? '–'
								: d.open
									? m.open()
									: m.closed()
							: celsius(d.measured_temperature)}
					</td>
					<td>{celsius(d.desired_temperature)}</td>
					<td>{modeName(d.mode)}</td>
					<td>{d.valve_position === undefined || d.type === 'thermostat' ? '–' : `${d.valve_position} %`}</td>
					<td>
						{#if d.low_battery}<span class="badge on">{m.badge_battery()}</span>{/if}
						{#if d.unconfirmed}
							<span class="badge" title={m.badge_unconfirmed_hint()}>{m.badge_unconfirmed({ n: d.unconfirmed })}</span>
						{/if}
						{#if d.rf_error}
							<span class="badge on" title={m.badge_no_answer_hint()}>{m.badge_no_answer()}</span>
						{/if}
					</td>
					<td class="muted heard">
						{ago(d.seen_seconds_ago)}{#if d.rssi !== undefined}{' '}<span class="dbm">· {d.rssi} dBm</span>{/if}
					</td>
				</tr>
			{/each}
		</tbody>
	</table>
</div>

<style>
	.heard {
		white-space: nowrap;
	}
	/* Secondary: there to compare reception, not to read first. */
	.dbm {
		opacity: 0.6;
		font-size: 0.85em;
	}
</style>
