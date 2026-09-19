import { createApp } from 'vue'
import { createPinia, setActivePinia } from 'pinia'
import { configureSessionStorage, useSessionStore } from '@avalon/api-client'
// Design tokens + base page styles come from the shared component library
// so every Avalon client (hub, mobile-hub) renders the same theme.
import '@avalon/ui/src/styles/tokens.css'
import '@avalon/ui/src/styles/global.css'
import App from './App.vue'
import router from './router'
import { createSecureSessionStorage } from './storage/secureStorage'

const app = createApp(App)
const pinia = createPinia()
setActivePinia(pinia)
app.use(pinia)

// Issue #60's whole point: the session token goes through the OS keychain
// (src-tauri/src/lib.rs's native command) on this platform, never
// `localStorage` — configured once, before the session store reads
// anything, same as apps/hub leaves this at its `localStorage` default.
configureSessionStorage(createSecureSessionStorage())

useSessionStore()
  .initialize()
  .then(() => {
    app.use(router).mount('#app')
  })
