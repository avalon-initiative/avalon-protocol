import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// Tauri expects a fixed dev-server port and a strict port so it can bind
// reliably across desktop and mobile targets.
export default defineConfig({
  plugins: [vue()],
  server: {
    port: 1420,
    strictPort: true,
  },
})
