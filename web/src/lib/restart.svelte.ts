import { invalidateAll } from '$app/navigation';
import { put, waitForBoard } from './api.svelte.js';

/** Settings that only apply after a restart: save them, then wait for the board. */
export const restart = $state<{ phase: '' | 'waiting' | 'back' | 'gone'; hostname: string }>({
	phase: '',
	hostname: ''
});

export async function saveAndRestart(patch: Record<string, unknown>, hostname: string) {
	await put('/settings', patch);
	restart.hostname = hostname;
	restart.phase = 'waiting';
	restart.phase = (await waitForBoard()) ? 'back' : 'gone';
	if (restart.phase === 'back') await invalidateAll();
}
