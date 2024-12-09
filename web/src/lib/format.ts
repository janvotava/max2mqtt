import type { DeviceType } from './types';

export const typeName: Record<DeviceType, string> = {
	cube: 'Cube',
	heater: 'Radiator thermostat',
	heater_plus: 'Radiator thermostat+',
	thermostat: 'Wall thermostat',
	shutter_contact: 'Window sensor',
	push_button: 'Eco switch'
};

export function ago(seconds: number | undefined): string {
	if (seconds === undefined) return 'not heard since boot';
	if (seconds < 60) return `${seconds} s ago`;
	if (seconds < 3600) return `${Math.floor(seconds / 60)} min ago`;
	return `${Math.floor(seconds / 3600)} h ago`;
}

export function duration(seconds: number): string {
	const d = Math.floor(seconds / 86400);
	const h = Math.floor((seconds % 86400) / 3600);
	const m = Math.floor((seconds % 3600) / 60);
	return d > 0 ? `${d} d ${h} h` : h > 0 ? `${h} h ${m} min` : `${m} min`;
}

export const celsius = (t: number | undefined) => (t === undefined ? '–' : `${t.toFixed(1)} °C`);
