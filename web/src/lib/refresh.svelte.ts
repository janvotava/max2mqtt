import { invalidate } from '$app/navigation';

/** Reload the status (root layout's load) every `seconds` while this page is shown. */
export function autoRefresh(seconds: number | (() => number) = 5) {
	$effect(() => {
		const period = typeof seconds === 'function' ? seconds() : seconds;
		const timer = setInterval(() => {
			if (document.visibilityState === 'visible') invalidate('app:status');
		}, period * 1000);
		return () => clearInterval(timer);
	});
}
