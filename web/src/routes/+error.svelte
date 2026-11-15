<script lang="ts">
	import { m } from '#lib/paraglide/messages.js';
	import { page } from '$app/state';
</script>

<h1>{page.status === 404 ? m.not_found() : m.error()}</h1>
<p class="error">{page.error?.message}</p>
<p><a href="#/">{m.back_overview()}</a></p>
