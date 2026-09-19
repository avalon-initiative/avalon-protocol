import { defineConfig } from 'vitest/config'

// Same jsdom/localStorage setup as apps/hub/vite.config.ts's test block —
// this package's tests exercise the same localStorage-backed default
// storage adapter hub relies on.
export default defineConfig({
  test: {
    environment: 'jsdom',
  },
})
