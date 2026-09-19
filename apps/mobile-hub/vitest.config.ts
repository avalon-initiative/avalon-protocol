import { defineConfig } from 'vitest/config'
import vue from '@vitejs/plugin-vue'

// Separate from vite.config.ts's dev/build config (fixed port 1420 for
// Tauri) — same jsdom/localStorage setup apps/hub and packages/api-client
// use for their own test runs.
export default defineConfig({
  plugins: [vue()],
  test: {
    environment: 'jsdom',
  },
})
