import { error } from '@sveltejs/kit';
import { get } from '#lib/api.svelte.js';
import type { Crash, Settings } from '#lib/types.js';
import type { PageLoad } from './$types';

export const load: PageLoad = async ({ parent, depends }) => {
	depends('app:crashes');
	await parent();
	const [settings, crashes] = await Promise.all([
		get<Settings>('/settings').catch((e: Error) => error(503, e.message)),
		get<Crash[]>('/crashes').catch(() => [] as Crash[])
	]);
	return { settings, crashes };
};
