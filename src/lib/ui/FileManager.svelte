<script lang="ts">
  import { createEventDispatcher } from "svelte";
  import { FolderIcon, FileIcon, UploadIcon, DownloadIcon } from "svelte-feather-icons";
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
  let downloadProgress: Record<string, number> = {};
  let downloading: Record<string, boolean> = {};

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
    const url = `/api/files/download/${path}`;
    
    // 设置下载状态
    downloading[filename] = true;
    downloadProgress[filename] = 0;
    downloading = {...downloading};
    downloadProgress = {...downloadProgress};
    
    const xhr = new XMLHttpRequest();
    xhr.open('GET', url);
    xhr.responseType = 'blob';
    xhr.setRequestHeader('Session', sessionName);

    // 监听所有相关的进度事件
    xhr.addEventListener("loadstart", () => {
      downloadProgress[filename] = 0;
      downloadProgress = {...downloadProgress};
    });

    xhr.addEventListener("progress", (event) => {
      if (event.lengthComputable) {
        const percent = Math.round((event.loaded / event.total) * 100);
        downloadProgress[filename] = percent;
        downloadProgress = {...downloadProgress};
      }
    });

    xhr.addEventListener("load", () => {
      downloadProgress[filename] = 100;
      downloadProgress = {...downloadProgress};
    });
    
    // 使用 Promise 包装异步操作
    try {
      await new Promise((resolve, reject) => {
        xhr.onload = () => {
          if (xhr.status === 200) {
            const blob = xhr.response;
            const url = window.URL.createObjectURL(blob);
            const link = document.createElement('a');
            link.href = url;
            link.download = filename;
            
            document.body.appendChild(link);
            link.click();
            document.body.removeChild(link);
            window.URL.revokeObjectURL(url);
            resolve(undefined);
          } else {
            reject(new Error('Download failed'));
          }
        };
        
        xhr.onerror = () => {
          reject(new Error('Download failed'));
        };

        xhr.send();
      });
    } catch (error) {
      console.error('Download failed:', error);
    } finally {
      // 延迟一秒后清理下载状态
      setTimeout(() => {
        delete downloading[filename];
        delete downloadProgress[filename];
        downloading = {...downloading};
        downloadProgress = {...downloadProgress};
      }, 1000);
    }
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

<OverlayMenu
  title="File Manager"
  description="Manage your files and directories"
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
            <div class="h-1 bg-zinc-700 rounded-full overflow-hidden mb-2">
              <div 
                class="h-full bg-green-500 transition-all duration-200"
                style="width: {downloadProgress[filename]}%"
              />
            </div>
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
            <div class="p-4 flex items-center justify-between hover:bg-zinc-700/50">
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
  </div>
</OverlayMenu>

<style lang="postcss">
  button {
    @apply transition-colors duration-200;
  }
</style>
