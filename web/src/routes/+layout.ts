import { auth, get } from '#lib/api.svelte.js';
import type { Status } from '#lib/types.js';
import type { LayoutLoad } from './$types';

// The last good status, so a failed refresh (the board restarting, Wi-Fi hiccup) keeps
// showing it next to the error instead of an empty page.
let last: Status | null = null;

export const load: LayoutLoad = async ({ depends }) => {
	depends('app:status');
	if (auth.password === null) return { status: null, statusError: '' };
	try {
		last = await get<Status>('/status');
		return { status: last, statusError: '' };
	} catch (e) {
		return { status: last, statusError: (e as Error).message };
	}
};
