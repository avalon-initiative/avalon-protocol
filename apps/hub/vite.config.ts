/// <reference types="vitest/config" />
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vitest/config'
import vue from '@vitejs/plugin-vue'

// The Hub is a client only — it holds no protocol state of its own. It talks
// to avalon-server over HTTP/WebSocket at whatever AVALON_SERVER_URL points
// to; there is no bundled backend here.

const dirName = dirname(fileURLToPath(import.meta.url))

// Issue #232: docs/trusted-networks.json (repo root) is the one canonical,
// publicly-published trust-anchor list — the README renders it and the Hub
// bundles it, but neither is a hand-maintained second copy. Since it lives
// outside apps/hub's own `src` (so `vue-tsc`'s project include and Vite's
// module graph both stay simple), this synchronously mirrors it into
// `src/generated/` every time this config loads — dev server start, build,
// and `vitest` alike — so `src/generated/trusted-networks.json` is always a
// fresh copy of the real source of truth, never edited by hand (it's
// gitignored; see .gitignore). `src/network/trustAnchors.ts` imports the
// generated copy directly.
const canonicalTrustedNetworksPath = resolve(dirName, '../../docs/trusted-networks.json')
const generatedTrustedNetworksPath = resolve(dirName, 'src/generated/trusted-networks.json')
mkdirSync(dirname(generatedTrustedNetworksPath), { recursive: true })
writeFileSync(generatedTrustedNetworksPath, readFileSync(canonicalTrustedNetworksPath))

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
