/// <reference types="vitest/config" />
import { defineConfig } from 'vitest/config'
import vue from '@vitejs/plugin-vue'
import { generateTrustedNetworks } from './scripts/generate-trusted-networks.mjs'

// The Hub is a client only — it holds no protocol state of its own. It talks
// to avalon-server over HTTP/WebSocket at whatever AVALON_SERVER_URL points
// to; there is no bundled backend here.

// Issue #232 (`src/generated/trusted-networks.json`, gitignored, mirrors
// docs/trusted-networks.json) needs to exist before `vue-tsc -b` type-checks
// `src/network/trustAnchors.ts`'s import of it — `vue-tsc` runs ahead of
// Vite in `npm run build`, so this config loading is too late for that path.
// `package.json`'s `prebuild` script covers `build`; this call keeps `dev`
// and `test` (which load this config directly, no separate typecheck step)
// covered too.
generateTrustedNetworks()

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
