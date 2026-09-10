/// <reference types="vitest/config" />
import { execSync } from 'node:child_process'
import { defineConfig } from 'vitest/config'
import vue from '@vitejs/plugin-vue'
import { generateTrustedNetworks } from './scripts/generate-trusted-networks.mjs'

// The Hub is a client only — it holds no protocol state of its own. It talks
// to avalon-server over HTTP/WebSocket at whatever AVALON_SERVER_URL points
// to; there is no bundled backend here.

// Also called here (not just from package.json's prebuild) since dev/test load this config directly.
generateTrustedNetworks()

// Footer build info: the exact git tag when built from a release, otherwise
// the short commit hash for dev/CI builds — mirrors mobile-hub's Tauri
// `get_app_info` split between `releaseVersion` and `buildRevision`.
function getBuildRevision(): { revision: string; isRelease: boolean } {
  try {
    const tag = execSync('git describe --tags --exact-match', {
      stdio: ['ignore', 'pipe', 'ignore'],
    })
      .toString()
      .trim()
    return { revision: tag, isRelease: true }
  } catch {
    try {
      const hash = execSync('git rev-parse --short HEAD', { stdio: ['ignore', 'pipe', 'ignore'] })
        .toString()
        .trim()
      return { revision: hash, isRelease: false }
    } catch {
      return { revision: 'unknown', isRelease: false }
    }
  }
}

const { revision: buildRevision, isRelease: buildIsRelease } = getBuildRevision()

export default defineConfig({
  define: {
    __BUILD_REVISION__: JSON.stringify(buildRevision),
    __BUILD_IS_RELEASE__: JSON.stringify(buildIsRelease),
  },
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
