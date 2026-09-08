import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// Tauri expects a fixed dev-server port and a strict port so it can bind
// reliably across desktop and mobile targets.
export default defineConfig({
  plugins: [vue()],
  css: {
    preprocessorOptions: {
      // Silences Dart Sass's "legacy JS API" deprecation warning — see the
      // same setting in apps/hub/vite.config.ts.
      scss: {
        api: 'modern-compiler',
      },
    },
  },
  server: {
    port: 1420,
    strictPort: true,
  },
})
