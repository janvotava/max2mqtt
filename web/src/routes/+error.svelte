<script lang="ts">
	import { page } from '$app/state';
</script>

<h1>{page.status === 404 ? 'Not found' : 'Error'}</h1>
<p class="error">{page.error?.message}</p>
<p><a href="#/">← Overview</a></p>
