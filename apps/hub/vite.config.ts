/// <reference types="vitest/config" />
import { defineConfig } from 'vitest/config'
import vue from '@vitejs/plugin-vue'

// The Hub is a client only — it holds no protocol state of its own. It talks
// to avalon-server over HTTP/WebSocket at whatever AVALON_SERVER_URL points
// to; there is no bundled backend here.
export default defineConfig({
  plugins: [vue()],
  css: {
    preprocessorOptions: {
      // Silences Dart Sass's "legacy JS API" deprecation warning — the
      // `sass` package still defaults to its old render() API unless told
      // otherwise. No functional change, just picks the API Sass itself
      // recommends going forward.
      scss: {
        api: 'modern-compiler',
      },
    },
  },
  // jsdom's `environment` gives tests a real, working `localStorage` — but
  // only if Node's own built-in localStorage global (Node 20.16+) doesn't
  // shadow it first. `npm run test`'s NODE_OPTIONS=--no-experimental-webstorage
  // disables that, since Node's version throws without a --localstorage-file
  // path and otherwise wins the shadowing race against jsdom's.
  test: {
    environment: 'jsdom',
  },
})
