<script lang="ts">
	import { ago, celsius, typeName } from './format.js';
	import type { DeviceStatus } from './types.js';

	let { devices, showRoom = false }: { devices: DeviceStatus[]; showRoom?: boolean } = $props();
</script>

<div class="scroll">
	<table>
		<thead>
			<tr>
				<th>Name</th>
				{#if showRoom}<th>Room</th>{/if}
				<th>Type</th><th>Now</th><th>Target</th><th>Mode</th><th>Valve</th><th></th><th>Heard</th>
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
									? 'open'
									: 'closed'
							: celsius(d.measured_temperature)}
					</td>
					<td>{celsius(d.desired_temperature)}</td>
					<td>{d.mode ?? '–'}</td>
					<td>{d.valve_position === undefined || d.type === 'thermostat' ? '–' : `${d.valve_position} %`}</td>
					<td>
						{#if d.low_battery}<span class="badge on">battery</span>{/if}
						{#if d.unconfirmed}
							<span class="badge" title="Settings sent but not acknowledged yet; sent again when the device is heard."
								>{d.unconfirmed} unconfirmed</span
							>
						{/if}
						{#if d.rf_error}
							<span class="badge on" title="The device got no answer from a link partner. Re-link its room, or check the partner's batteries.">no answer</span>
						{/if}
					</td>
					<td class="muted" title={d.rssi === undefined ? '' : `${d.rssi} dBm`}>{ago(d.seen_seconds_ago)}</td>
				</tr>
			{/each}
		</tbody>
	</table>
</div>
