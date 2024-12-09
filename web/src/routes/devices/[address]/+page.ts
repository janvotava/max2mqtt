import { error } from '@sveltejs/kit';
import { get } from '#lib/api.svelte.js';
import type { DeviceConfig } from '#lib/types.js';
import type { PageLoad } from './$types';

// Only this device's settings, and only when they change (`app:device`): the whole
// bridge config is tens of kilobytes the board would have to build every few seconds.
export const load: PageLoad = async ({ params, parent, depends }) => {
	depends('app:device');
	await parent();
	const device = await get<DeviceConfig>(`/devices/${params.address}`).catch((e: Error) => error(404, e.message));
	return { device };
};
