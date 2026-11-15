import { m } from './paraglide/messages.js';
// API client. The password lives in sessionStorage (or localStorage with "remember me")
// and goes along as HTTP Basic auth. A 401 drops it, which shows the login form again.

const KEY = 'max2mqtt-password';

function stored(): string | null {
	try {
		return sessionStorage.getItem(KEY) ?? localStorage.getItem(KEY);
	} catch {
		return null;
	}
}

export const auth = $state<{ password: string | null }>({ password: stored() });

export function login(password: string, remember: boolean) {
	auth.password = password;
	try {
		(remember ? localStorage : sessionStorage).setItem(KEY, password);
	} catch {
		// Private mode: stays in memory only.
	}
}

export function logout() {
	auth.password = null;
	try {
		sessionStorage.removeItem(KEY);
		localStorage.removeItem(KEY);
	} catch {
		// nothing stored
	}
}

export class ApiError extends Error {
	constructor(
		public status: number,
		message: string
	) {
		super(message);
	}
}

export async function api<T>(method: string, path: string, body?: unknown, timeoutMs = 20000): Promise<T> {
	const headers: Record<string, string> = {};
	if (auth.password !== null) headers.Authorization = 'Basic ' + btoa('admin:' + auth.password);
	if (body !== undefined) headers['Content-Type'] = 'application/json';
	let response: Response | undefined;
	// The board has few connections: a request that finds them all busy is refused, so try
	// once more (not for writes, which might have been carried out).
	for (let attempt = 0; attempt < (method === 'GET' ? 2 : 1) && !response; attempt++) {
		if (attempt) await new Promise((r) => setTimeout(r, 400));
		try {
			response = await fetch('/api' + path, {
				method,
				headers,
				body: body === undefined ? undefined : JSON.stringify(body),
				signal: AbortSignal.timeout(timeoutMs)
			});
		} catch {
			// next attempt, or give up below
		}
	}
	if (!response) throw new ApiError(0, m.board_no_answer());
	const data = await response.json().catch(() => ({}));
	if (response.status === 401) logout();
	if (!response.ok) throw new ApiError(response.status, data.error ?? response.statusText);
	return data as T;
}

export const get = <T>(path: string, timeoutMs?: number) => api<T>('GET', path, undefined, timeoutMs);
export const post = <T = { ok: boolean }>(path: string, body: unknown = {}) => api<T>('POST', path, body);
export const put = <T = { ok: boolean }>(path: string, body: unknown) => api<T>('PUT', path, body);
export const del = <T = { ok: boolean }>(path: string) => api<T>('DELETE', path);

/** Upload a firmware image. XMLHttpRequest, because fetch cannot report upload progress. */
export function uploadFirmware(file: File, onProgress: (fraction: number) => void): Promise<void> {
	return new Promise((resolve, reject) => {
		const xhr = new XMLHttpRequest();
		xhr.open('POST', '/api/firmware');
		if (auth.password !== null) xhr.setRequestHeader('Authorization', 'Basic ' + btoa('admin:' + auth.password));
		xhr.setRequestHeader('Content-Type', 'application/octet-stream');
		xhr.upload.onprogress = (e) => e.lengthComputable && onProgress(e.loaded / e.total);
		xhr.onload = () => {
			if (xhr.status === 200) return resolve();
			let message = xhr.statusText;
			try {
				message = JSON.parse(xhr.responseText).error ?? message;
			} catch {
				// not JSON
			}
			if (xhr.status === 401) logout();
			reject(new ApiError(xhr.status, message));
		};
		xhr.onerror = () => reject(new ApiError(0, m.upload_interrupted()));
		xhr.send(file);
	});
}

/** After a restart: poll until the board answers again. */
export async function waitForBoard(maxSeconds = 60): Promise<boolean> {
	await new Promise((r) => setTimeout(r, 3000));
	for (let i = 0; i < maxSeconds / 2; i++) {
		try {
			await get('/status', 2000);
			return true;
		} catch {
			await new Promise((r) => setTimeout(r, 2000));
		}
	}
	return false;
}
