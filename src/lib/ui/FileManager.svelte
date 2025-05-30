<script lang="ts">
  import { createEventDispatcher } from "svelte";
  import { FolderIcon, FileIcon, UploadIcon, DownloadIcon, CheckIcon } from "svelte-feather-icons";
  import OverlayMenu from "./OverlayMenu.svelte";

  export let open: boolean;
  export let sessionName: string;

  const dispatch = createEventDispatcher();

  let currentPath = "";
  let files: Array<{
    name: string;
    is_directory: boolean;
    size: number;
    modified_time: number;
  }> = [];
  let loading = false;
  let uploadInput: HTMLInputElement;
  let uploadProgress: Record<string, number> = {};
  let uploading: Record<string, boolean> = {};
  let downloading: Record<string, boolean> = {};
  
  // 右键菜单相关状态
  let contextMenuVisible = false;
  let contextMenuX = 0;
  let contextMenuY = 0;
  let selectedFile = "";
  let showCopiedMessage = false;

  // 关闭右键菜单
  function closeContextMenu() {
    contextMenuVisible = false;
  }

  // 处理右键点击
  function handleContextMenu(event: MouseEvent, filename: string) {
    event.preventDefault();
    contextMenuX = event.clientX;
    contextMenuY = event.clientY;
    selectedFile = filename;
    contextMenuVisible = true;
  }

  // 复制curl命令
  async function copyCurlCommand() {
    const downloadPath = `${currentPath}/${selectedFile}`.replace(/\/\//g, '/');
    const encodedPath = downloadPath.split('/').map(part => encodeURIComponent(part)).join('/');
    const curlCommand = `curl -X GET "http://localhost:8051/api/files/download/${encodedPath}" -H "Session: ${sessionName}" --output "${selectedFile}"`;
    
    try {
      await navigator.clipboard.writeText(curlCommand);
      showCopiedMessage = true;
      setTimeout(() => {
        showCopiedMessage = false;
      }, 2000);
    } catch (error) {
      console.error('Failed to copy curl command:', error);
    }
    closeContextMenu();
  }

  // 浏览器直接下载
  function browserDownload() {
    handleDownload(selectedFile);
    closeContextMenu();
  }

  // 点击其他地方关闭右键菜单
  function handleGlobalClick() {
    if (contextMenuVisible) {
      closeContextMenu();
    }
  }

  async function loadDirectory(path: string) {
    if (path === "" || path === undefined) {
      path = "/";
    } else if (!path.startsWith("/")) {
      path = "/" + path;
    }
    
    loading = true;
    try {
      const response = await fetch(`/api/files/list/${path}`, {
        headers: {
          Session: sessionName,
        },
      });
      if (!response.ok) throw new Error("Failed to load directory");
      const data = await response.json();
      files = data.files;
      currentPath = path;
    } catch (error) {
      console.error("Failed to load directory:", error);
    } finally {
      loading = false;
    }
  }

  async function handlePathClick(path: string) {
    await loadDirectory(path);
  }

  async function handleUpload(event: Event) {
    const input = event.target as HTMLInputElement;
    if (!input.files?.length) return;

    const file = input.files[0];
    const uploadPath = `${currentPath}/${file.name}`.replace(/\/\//g, '/');
    
    // 设置上传状态
    uploading[file.name] = true;
    uploadProgress[file.name] = 0;
    uploading = {...uploading};
    uploadProgress = {...uploadProgress};

    const formData = new FormData();
    formData.append("path", uploadPath);
    formData.append("file", file);

    return new Promise((resolve, reject) => {
      const xhr = new XMLHttpRequest();
      
      xhr.upload.addEventListener("progress", (e) => {
        if (e.lengthComputable) {
          uploadProgress[file.name] = (e.loaded / e.total) * 100;
          uploadProgress = {...uploadProgress};
        }
      });

      xhr.addEventListener("load", async () => {
        if (xhr.status === 200) {
          await loadDirectory(currentPath);
          resolve(undefined);
        } else {
          reject(new Error("Upload failed"));
        }
      });

      xhr.addEventListener("error", () => {
        reject(new Error("Upload failed"));
      });

      xhr.addEventListener("loadend", () => {
        delete uploading[file.name];
        delete uploadProgress[file.name];
        uploading = {...uploading};
        uploadProgress = {...uploadProgress};
      });

      xhr.open("POST", "/api/files/upload");
      xhr.setRequestHeader("Session", sessionName);
      xhr.send(formData);
    });
  }

  async function handleDownload(filename: string) {
    const path = currentPath + "/" + filename;
    const downloadUrl = `/api/files/download/${path}`;
    
    // 设置下载状态
    downloading[filename] = true;
    downloading = {...downloading};

    return new Promise((resolve, reject) => {
      const xhr = new XMLHttpRequest();
      xhr.open('GET', downloadUrl);
      xhr.responseType = 'blob';
      xhr.setRequestHeader('Session', sessionName);

      // 下载完成
      xhr.addEventListener('load', () => {
        if (xhr.status === 200) {
          const blob = xhr.response;
          const blobUrl = window.URL.createObjectURL(blob);
          const link = document.createElement('a');
          link.href = blobUrl;
          link.download = filename;
          
          document.body.appendChild(link);
          link.click();
          document.body.removeChild(link);
          window.URL.revokeObjectURL(blobUrl);
          resolve(undefined);
        } else {
          reject(new Error('Download failed'));
        }
      });

      // 下载出错
      xhr.addEventListener('error', () => {
        reject(new Error('Download failed'));
      });

      // 开始下载
      xhr.send();
    }).catch(error => {
      console.error('Download failed:', error);
    }).finally(() => {
      // 延迟一秒后清理下载状态
      setTimeout(() => {
        delete downloading[filename];
        downloading = {...downloading};
      }, 1000);
    });
  }

  // 当组件打开时加载根目录
  $: if (open) {
    loadDirectory("");
  }

  function formatFileSize(size: number): string {
    if (size < 1024) return size + " B";
    if (size < 1024 * 1024) return (size / 1024).toFixed(1) + " KB";
    if (size < 1024 * 1024 * 1024) return (size / (1024 * 1024)).toFixed(1) + " MB";
    return (size / (1024 * 1024 * 1024)).toFixed(1) + " GB";
  }

  function formatDate(timestamp: number): string {
    return new Date(timestamp * 1000).toLocaleString();
  }
</script>

<svelte:window on:click={handleGlobalClick} />

<OverlayMenu
  title="文件临时传递工具"
  description="大文件(500M)右键复制curl命令,否则无法保证正常传输,这是浏览器的限制"
  showCloseButton
  {open}
  on:close
>
  <div class="flex flex-col gap-4">
    <!-- 路径导航 -->
    <div class="flex items-center gap-2 text-zinc-400">
      <button 
        class="hover:text-zinc-200" 
        on:click={() => handlePathClick("/")}
      >
        Root
      </button>
      {#each currentPath.split("/").filter(p => p) as pathPart, i}
        <span>/</span>
        <button 
          class="hover:text-zinc-200"
          on:click={() => handlePathClick(
            "/" + currentPath
              .split("/")
              .filter(p => p)
              .slice(0, i + 1)
              .join("/")
          )}
        >
          {pathPart}
        </button>
      {/each}
    </div>

    <!-- 上传按钮 -->
    <div class="flex justify-end gap-4 items-center">
      {#if Object.keys(uploading).length > 0 || Object.keys(downloading).length > 0}
        <div class="flex-1 max-w-xs">
          {#each Object.entries(uploading) as [filename, _]}
            <div class="text-sm text-zinc-400 mb-1">{filename} (上传中)</div>
            <div class="h-1 bg-zinc-700 rounded-full overflow-hidden mb-2">
              <div 
                class="h-full bg-indigo-500 transition-all duration-200"
                style="width: {uploadProgress[filename]}%"
              />
            </div>
          {/each}
          {#each Object.entries(downloading) as [filename, _]}
            <div class="text-sm text-zinc-400 mb-1">{filename} (下载中)</div>
          {/each}
        </div>
      {/if}
      <button
        class="px-3 py-1.5 bg-indigo-600 hover:bg-indigo-700 rounded-md text-sm flex items-center gap-2"
        on:click={() => uploadInput.click()}
      >
        <UploadIcon size="14" />
        Upload File
      </button>
      <input
        type="file"
        class="hidden"
        bind:this={uploadInput}
        on:change={handleUpload}
      />
    </div>

    <!-- 文件列表 -->
    <div class="bg-zinc-800/25 rounded-lg">
      {#if loading}
        <div class="p-4 text-center text-zinc-400">Loading...</div>
      {:else if files.length === 0}
        <div class="p-4 text-center text-zinc-400">Empty directory</div>
      {:else}
        <div class="divide-y divide-zinc-700">
          {#each files as file}
            <div 
              class="p-4 flex items-center justify-between hover:bg-zinc-700/50"
              on:contextmenu={(e) => !file.is_directory && handleContextMenu(e, file.name)}
            >
              <div class="flex items-center gap-3">
                {#if file.is_directory}
                  <FolderIcon class="text-indigo-400" size="20" />
                  <button 
                    class="hover:text-zinc-200" 
                    on:click={() => handlePathClick(currentPath + "/" + file.name)}
                  >
                    {file.name}
                  </button>
                {:else}
                  <FileIcon class="text-zinc-400" size="20" />
                  <span>{file.name}</span>
                {/if}
              </div>
              <div class="flex items-center gap-4">
                <span class="text-sm text-zinc-400">
                  {formatFileSize(file.size)}
                </span>
                <span class="text-sm text-zinc-400">
                  {formatDate(file.modified_time)}
                </span>
                {#if !file.is_directory}
                  <button
                    class="p-1 hover:bg-zinc-600 rounded"
                    on:click={() => handleDownload(file.name)}
                  >
                    <DownloadIcon class="text-zinc-400" size="16" />
                  </button>
                {/if}
              </div>
            </div>
          {/each}
        </div>
      {/if}
    </div>

    <!-- 右键菜单 -->
    {#if contextMenuVisible}
      <div 
        class="fixed bg-zinc-800 rounded-lg shadow-lg py-2 z-50"
        style="left: {contextMenuX}px; top: {contextMenuY}px;"
      >
        <button 
          class="w-full px-4 py-2 text-left hover:bg-zinc-700 text-sm"
          on:click={copyCurlCommand}
        >
          Curl下载命令
        </button>
        <button 
          class="w-full px-4 py-2 text-left hover:bg-zinc-700 text-sm"
          on:click={browserDownload}
        >
          浏览器直接下载
        </button>
      </div>
    {/if}

    <!-- 复制成功提示 -->
    {#if showCopiedMessage}
      <div class="fixed bottom-4 right-4 bg-green-500 text-white px-4 py-2 rounded-lg flex items-center gap-2">
        <CheckIcon size="16" />
        curl命令已复制
      </div>
    {/if}
  </div>
</OverlayMenu>

<style lang="postcss">
  button {
    @apply transition-colors duration-200;
  }
</style>
