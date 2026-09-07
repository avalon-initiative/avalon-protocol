import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// The Hub is a client only — it holds no protocol state of its own. It talks
// to avalon-server over HTTP/WebSocket at whatever AVALON_SERVER_URL points
// to; there is no bundled backend here.
export default defineConfig({
  plugins: [vue()],
})
