import { m } from './paraglide/messages.js';
import { getLocale } from './paraglide/runtime.js';
import type { DeviceType } from './types';

// Getters: messages are looked up in the current language each time.
export const typeName: Record<DeviceType, string> = {
	get cube() {
		return m.type_cube();
	},
	get heater() {
		return m.type_heater();
	},
	get heater_plus() {
		return m.type_heater_plus();
	},
	get thermostat() {
		return m.type_thermostat();
	},
	get shutter_contact() {
		return m.type_shutter_contact();
	},
	get push_button() {
		return m.type_push_button();
	}
};

const MODES: Record<string, () => string> = {
	auto: m.mode_auto,
	manual: m.mode_manual,
	temporary: m.mode_temporary,
	boost: m.mode_boost
};
/** A device mode for display; `–` when unknown. */
export const modeName = (mode: string | undefined) => (mode === undefined ? '–' : (MODES[mode]?.() ?? mode));

export function ago(seconds: number | undefined): string {
	if (seconds === undefined) return m.not_heard();
	if (seconds < 60) return m.ago_s({ n: seconds });
	if (seconds < 3600) return m.ago_min({ n: Math.floor(seconds / 60) });
	return m.ago_h({ n: Math.floor(seconds / 3600) });
}

export function duration(seconds: number): string {
	const d = Math.floor(seconds / 86400);
	const h = Math.floor((seconds % 86400) / 3600);
	const min = Math.floor((seconds % 3600) / 60);
	return d > 0 ? m.dur_days({ d, h }) : h > 0 ? m.dur_hours({ h, m: min }) : m.dur_minutes({ m: min });
}

export const celsius = (t: number | undefined) =>
	t === undefined ? '–' : `${t.toLocaleString(getLocale(), { minimumFractionDigits: 1, maximumFractionDigits: 1 })} °C`;
