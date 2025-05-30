// vite.config.ts
import { execSync } from "node:child_process";
import { defineConfig } from "file:///opt/github/gamer/mycode/sshx/node_modules/vite/dist/node/index.js";
import { sveltekit } from "file:///opt/github/gamer/mycode/sshx/node_modules/@sveltejs/kit/src/exports/vite/index.js";
var commitHash = execSync("git rev-parse --short HEAD").toString().trim();
var vite_config_default = defineConfig({
  define: {
    __APP_VERSION__: JSON.stringify("0.4.1-" + commitHash)
  },
  plugins: [sveltekit()],
  server: {
    proxy: {
      "/api": {
        target: "http://127.0.0.1:8051",
        changeOrigin: true,
        ws: true
      }
    }
  }
});
export {
  vite_config_default as default
};
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsidml0ZS5jb25maWcudHMiXSwKICAic291cmNlc0NvbnRlbnQiOiBbImNvbnN0IF9fdml0ZV9pbmplY3RlZF9vcmlnaW5hbF9kaXJuYW1lID0gXCIvb3B0L2dpdGh1Yi9nYW1lci9teWNvZGUvc3NoeFwiO2NvbnN0IF9fdml0ZV9pbmplY3RlZF9vcmlnaW5hbF9maWxlbmFtZSA9IFwiL29wdC9naXRodWIvZ2FtZXIvbXljb2RlL3NzaHgvdml0ZS5jb25maWcudHNcIjtjb25zdCBfX3ZpdGVfaW5qZWN0ZWRfb3JpZ2luYWxfaW1wb3J0X21ldGFfdXJsID0gXCJmaWxlOi8vL29wdC9naXRodWIvZ2FtZXIvbXljb2RlL3NzaHgvdml0ZS5jb25maWcudHNcIjtpbXBvcnQgeyBleGVjU3luYyB9IGZyb20gXCJub2RlOmNoaWxkX3Byb2Nlc3NcIjtcblxuaW1wb3J0IHsgZGVmaW5lQ29uZmlnIH0gZnJvbSBcInZpdGVcIjtcbmltcG9ydCB7IHN2ZWx0ZWtpdCB9IGZyb20gXCJAc3ZlbHRlanMva2l0L3ZpdGVcIjtcblxuY29uc3QgY29tbWl0SGFzaCA9IGV4ZWNTeW5jKFwiZ2l0IHJldi1wYXJzZSAtLXNob3J0IEhFQURcIikudG9TdHJpbmcoKS50cmltKCk7XG5cbmV4cG9ydCBkZWZhdWx0IGRlZmluZUNvbmZpZyh7XG4gIGRlZmluZToge1xuICAgIF9fQVBQX1ZFUlNJT05fXzogSlNPTi5zdHJpbmdpZnkoXCIwLjQuMS1cIiArIGNvbW1pdEhhc2gpLFxuICB9LFxuXG4gIHBsdWdpbnM6IFtzdmVsdGVraXQoKV0sXG5cbiAgc2VydmVyOiB7XG4gICAgcHJveHk6IHtcbiAgICAgIFwiL2FwaVwiOiB7XG4gICAgICAgIHRhcmdldDogXCJodHRwOi8vMTI3LjAuMC4xOjgwNTFcIixcbiAgICAgICAgY2hhbmdlT3JpZ2luOiB0cnVlLFxuICAgICAgICB3czogdHJ1ZSxcbiAgICAgIH0sXG4gICAgfSxcbiAgfSxcbn0pO1xuIl0sCiAgIm1hcHBpbmdzIjogIjtBQUF5USxTQUFTLGdCQUFnQjtBQUVsUyxTQUFTLG9CQUFvQjtBQUM3QixTQUFTLGlCQUFpQjtBQUUxQixJQUFNLGFBQWEsU0FBUyw0QkFBNEIsRUFBRSxTQUFTLEVBQUUsS0FBSztBQUUxRSxJQUFPLHNCQUFRLGFBQWE7QUFBQSxFQUMxQixRQUFRO0FBQUEsSUFDTixpQkFBaUIsS0FBSyxVQUFVLFdBQVcsVUFBVTtBQUFBLEVBQ3ZEO0FBQUEsRUFFQSxTQUFTLENBQUMsVUFBVSxDQUFDO0FBQUEsRUFFckIsUUFBUTtBQUFBLElBQ04sT0FBTztBQUFBLE1BQ0wsUUFBUTtBQUFBLFFBQ04sUUFBUTtBQUFBLFFBQ1IsY0FBYztBQUFBLFFBQ2QsSUFBSTtBQUFBLE1BQ047QUFBQSxJQUNGO0FBQUEsRUFDRjtBQUNGLENBQUM7IiwKICAibmFtZXMiOiBbXQp9Cg==
