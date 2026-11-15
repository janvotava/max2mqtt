<script lang="ts">
	import { m } from '#lib/paraglide/messages.js';
	import { getLocale, locales, setLocale } from '#lib/paraglide/runtime.js';
	import { invalidateAll } from '$app/navigation';
	import { page } from '$app/state';
	import { auth, login, logout, get, ApiError } from '#lib/api.svelte.js';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();

	let password = $state('');
	let remember = $state(false);
	let error = $state('');
	let busy = $state(false);

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		busy = true;
		error = '';
		login(password, remember);
		try {
			await get('/status');
			password = '';
			await invalidateAll();
		} catch (err) {
			error = err instanceof ApiError && err.status === 401 ? m.wrong_password() : (err as Error).message;
		} finally {
			busy = false;
		}
	}

	const links = [
		{ href: '#/', path: '/', label: m.nav_devices() },
		{ href: '#/settings', path: '/settings', label: m.nav_settings() },
		{ href: '#/logs', path: '/logs', label: m.nav_log() }
	];
	// For screen readers and hyphenation; setLocale reloads the page with the new one.
	$effect(() => {
		document.documentElement.lang = getLocale();
	});
	// The route, not the URL: with hash routing the page's path is in the hash.
	// Device pages belong to the device list (`/`).
	const current = (path: string) =>
		path === '/' ? page.route.id === '/' || !!page.route.id?.startsWith('/devices') : !!page.route.id?.startsWith(path);
</script>

<header>
	<!-- One baseline: centring boxes of different font sizes left the switch looking high. -->
	<div class="start">
		<a class="brand" href="#/">max2mqtt</a>
		<span class="lang" role="group" aria-label={m.language()}>
			{#each locales as l (l)}
				<button class="link" aria-pressed={l === getLocale()} onclick={() => setLocale(l)}>{l.toUpperCase()}</button>
			{/each}
		</span>
	</div>
	{#if auth.password !== null}
		<nav>
			{#each links as l (l.href)}
				<a href={l.href} aria-current={current(l.path) ? 'page' : undefined}>{l.label}</a>
			{/each}
			<button class="link" onclick={logout}>{m.log_out()}</button>
		</nav>
	{/if}
</header>

<main>
	{#if auth.password === null}
		<form class="card login" onsubmit={submit}>
			<h1>{m.log_in()}</h1>
			<p class="muted">{m.login_forgot()}</p>
			<label>
				{m.password()}
				<!-- svelte-ignore a11y_autofocus -->
				<input type="password" bind:value={password} autocomplete="current-password" required autofocus />
			</label>
			<label class="check"><input type="checkbox" bind:checked={remember} /> {m.remember()}</label>
			{#if error}<p class="error">{error}</p>{/if}
			<button disabled={busy}>{busy ? m.checking() : m.log_in()}</button>
		</form>
	{:else}
		{@render children()}
	{/if}
</main>

<style>
	:global {
		:root {
			--bg: #f7f6fb;
			--card: #fff;
			--text: #1f1b2e;
			--muted: #6e6880;
			--line: #e6e3ef;
			--accent: #6741d9;
			--accent-soft: #f0ebff;
			--accent-text: #fff;
			--ok: #2b8a3e;
			--warn: #c92a2a;
			--radius: 10px;
			font-family:
				system-ui,
				-apple-system,
				'Segoe UI',
				Roboto,
				sans-serif;
			font-size: 15px;
			line-height: 1.45;
			color: var(--text);
			background: var(--bg);
		}
		@media (prefers-color-scheme: dark) {
			:root {
				--bg: #141220;
				--card: #1d1a2c;
				--text: #e8e5f2;
				--muted: #958fab;
				--line: #2d2942;
				--accent: #9775fa;
				--accent-soft: #2a2245;
				--accent-text: #141220;
				--ok: #51cf66;
				--warn: #ff6b6b;
			}
		}
		* {
			box-sizing: border-box;
		}
		body {
			margin: 0;
		}
		h1 {
			font-size: 1.35rem;
			margin: 0 0 0.75rem;
		}
		h2 {
			font-size: 1.05rem;
			margin: 0 0 0.75rem;
		}
		a {
			color: inherit;
		}
		.card {
			background: var(--card);
			border: 1px solid var(--line);
			border-radius: var(--radius);
			padding: 1rem 1.1rem;
			margin-bottom: 1rem;
		}
		.muted {
			color: var(--muted);
		}
		.error {
			color: var(--warn);
		}
		.ok {
			color: var(--ok);
		}
		label {
			display: grid;
			gap: 0.25rem;
			margin-bottom: 0.75rem;
			font-size: 0.9rem;
			color: var(--muted);
		}
		label.check {
			display: flex;
			align-items: center;
			gap: 0.5rem;
			color: var(--text);
		}
		input,
		select {
			font: inherit;
			color: var(--text);
			background: var(--bg);
			border: 1px solid var(--line);
			border-radius: 6px;
			padding: 0.45rem 0.6rem;
			width: 100%;
		}
		input[type='checkbox'] {
			width: auto;
		}
		button {
			font: inherit;
			border: 0;
			border-radius: 6px;
			padding: 0.5rem 1rem;
			background: var(--accent);
			color: var(--accent-text);
			cursor: pointer;
		}
		button:disabled {
			opacity: 0.6;
			cursor: default;
		}
		button.secondary {
			background: transparent;
			color: var(--text);
			border: 1px solid var(--line);
		}
		button.danger {
			background: var(--warn);
		}
		button.link {
			background: none;
			color: var(--muted);
			padding: 0;
		}
		.grid {
			display: grid;
			grid-template-columns: repeat(auto-fit, minmax(14rem, 1fr));
			gap: 0 1rem;
		}
		.row {
			display: flex;
			gap: 0.5rem;
			flex-wrap: wrap;
			align-items: center;
		}
		table {
			width: 100%;
			border-collapse: collapse;
		}
		th,
		td {
			text-align: left;
			padding: 0.45rem 0.5rem;
			border-bottom: 1px solid var(--line);
			white-space: nowrap;
		}
		th {
			font-weight: 500;
			color: var(--muted);
			font-size: 0.85rem;
		}
		.scroll {
			overflow-x: auto;
		}
		.badge {
			display: inline-block;
			font-size: 0.8rem;
			padding: 0.05rem 0.5rem;
			border-radius: 999px;
			border: 1px solid var(--line);
			color: var(--muted);
		}
		.badge.on {
			border-color: var(--accent);
			color: var(--accent);
		}
		.banner {
			background: var(--accent-soft);
			border-color: transparent;
		}
		.banner a {
			color: var(--accent);
			font-weight: 500;
		}
		dl {
			display: grid;
			grid-template-columns: max-content 1fr;
			gap: 0.3rem 1rem;
			margin: 0;
		}
		dt {
			color: var(--muted);
		}
		dd {
			margin: 0;
		}
	}
	header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
		padding: 0.7rem 1rem;
		background: var(--card);
		border-bottom: 1px solid var(--line);
		flex-wrap: wrap;
	}
	.brand {
		font-weight: 700;
		text-decoration: none;
		color: var(--accent);
	}
	.start {
		display: flex;
		align-items: baseline;
		gap: 1rem;
		margin-right: auto;
	}
	.lang {
		display: flex;
		align-items: baseline;
		gap: 0.4rem;
		font-size: 0.8rem;
	}
	.lang button[aria-pressed='true'] {
		color: var(--accent);
		font-weight: 600;
		text-decoration: none;
	}
	nav {
		display: flex;
		gap: 1rem;
		align-items: center;
	}
	nav a {
		text-decoration: none;
		color: var(--muted);
	}
	nav a[aria-current='page'] {
		color: var(--text);
		font-weight: 600;
	}
	main {
		max-width: 78rem;
		margin: 0 auto;
		padding: 1rem;
	}
	.login {
		max-width: 24rem;
		margin: 3rem auto;
	}
</style>
