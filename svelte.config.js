import adapter from "@sveltejs/adapter-static";
import preprocess from "svelte-preprocess";

/** @type {import('@sveltejs/kit').Config} */
const config = {
  // Consult https://github.com/sveltejs/svelte-preprocess
  // for more information about preprocessors
  preprocess: [
    preprocess({
      postcss: true,
    }),
  ],

  kit: {
    adapter: adapter({
      fallback: "spa.html", // SPA mode
      precompress: true,
    }),
    prerender: {
      handleHttpError: ({ path, message }) => {
        // 忽略特定错误或记录错误
        console.warn(`Prerender error at ${path}: ${message}`);
      }
    }
  },
};

export default config;
