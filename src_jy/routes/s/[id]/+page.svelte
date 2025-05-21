<script lang="ts">
  import { page } from "$app/stores";

  import Session from "$lib/Session.svelte";

  let title: string = "景阳雷诺 | 协助工具";
</script>

<svelte:head>
  <title>{title}</title>

  <style>
    body {
      overscroll-behavior: none;
    }
  </style>
</svelte:head>

<Session
  id={$page.params.id}
  on:receiveName={({ detail: sessionName }) => {
    if (sessionName) {
      title = `景阳雷诺 | 协助工具`;
    }
  }}
/>
