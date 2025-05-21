<script lang="ts">
  import { createEventDispatcher } from "svelte";
  import { page } from '$app/stores';
  import {
    SettingsIcon,
  } from "svelte-feather-icons";

  // export let hasWriteAccess: boolean | undefined;
  export let sessions: string[];
  // export let id: string;

  // export let sessions: string[];
  $:baseUrl = $page.url.origin;
  const dispatch = createEventDispatcher<{
    create: void;
    chat: void;
    settings: void;
    networkInfo: void;
  }>();
</script>

<div class="panel inline-block px-3 py-2">
  <div class="flex items-center select-none">
    <p class="ml-1.5 mr-2 font-medium">admin</p>
    <div class="v-divider" />
    <div class="flex space-x-1">
<!--      <button-->
<!--        class="icon-button"-->
<!--        on:click={() => dispatch("create")}-->
<!--        disabled={!connected}-->
<!--        title={!connected-->
<!--          ? "Not connected"-->
<!--          : hasWriteAccess === false // Only show the "No write access" title after confirming read-only mode.-->
<!--          ? "No write access"-->
<!--          : "Create new terminal"}-->
<!--      >-->
<!--        <PlusCircleIcon strokeWidth={1.5} class="p-0.5" />-->
<!--      </button>-->
<!--      <button class="icon-button" on:click={() => dispatch("chat")}>-->
<!--        <MessageSquareIcon strokeWidth={1.5} class="p-0.5" />-->
<!--        {#if newMessages}-->
<!--          <div class="activity" />-->
<!--        {/if}-->
<!--      </button>-->
      <button class="icon-button" on:click={() => dispatch("settings")}>
        <SettingsIcon strokeWidth={1.5} class="p-0.5" />
      </button>
    </div>
    <div class="v-divider" />
    <div class="flex space-x-1">
<!--      <button class="icon-button" on:click={() => dispatch("networkInfo")}>-->
<!--        <WifiIcon strokeWidth={1.5} class="p-0.5" />-->
<!--      </button>-->
    </div>
  </div>
  <div class="ml-1">
    {#if sessions && sessions.length > 0}
      <div class="space-y-2 mt-2">
        {#each sessions as session}
          <div class="text-sm">
            <button
              class="icon-button"
              on:click={() => {
                //dispatch("updateId",session)
                window.open(`${baseUrl}/s/${session}/#view`, '_blank')
              }}
            >
<!--              <PlusCircleIcon strokeWidth={1.5} class="p-0.5" />-->
              <div class="flex">
                连接到 {session}
              </div>
            </button>
          </div>
        {/each}
      </div>
    {/if}
  </div>
</div>

<style lang="postcss">
    .v-divider {
        @apply h-5 mx-2 border-l-4 border-zinc-800;
    }

    .icon-button {
        @apply relative rounded-md p-1 hover:bg-zinc-700 active:bg-indigo-700 transition-colors;
        @apply disabled:opacity-50 disabled:bg-transparent;
    }
</style>
