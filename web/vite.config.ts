import { paraglideVitePlugin } from '@inlang/paraglide-js';
import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

// The firmware serves exactly one file, `/`: build/index.html, gzipped and embedded by
// esp/build.rs. So: hash routing (every page is `/#/...`), and everything
// inlined into that one file.
export default defineConfig({
	plugins: [
		// English and Czech. The choice is kept in the browser; without one, the browser's
		// language decides. No URL strategy: the hash router owns the URL.
		paraglideVitePlugin({
			project: './project.inlang',
			outdir: './src/lib/paraglide',
			strategy: ['localStorage', 'preferredLanguage', 'baseLocale'],
			emitTsDeclarations: true
		}),
		sveltekit({
			compilerOptions: {
				// Force runes mode for the project, except for libraries. Can be removed in svelte 6.
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},
			adapter: adapter({ fallback: 'index.html' }),
			router: { type: 'hash' },
			output: { bundleStrategy: 'inline' }
		})
	],
	server: {
		// `web-dev`: the UI from Vite, the API from a real board.
		proxy: { '/api': process.env.DEVICE ?? 'http://max.lan' }
	}
});
