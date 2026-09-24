import { createApp } from 'vue'
import { createPinia, setActivePinia } from 'pinia'
import { configureSessionStorage } from './api/sessionStorage'
import { useSessionStore } from './api/session'
// Design tokens + base page styles come from the shared component library
// so every Avalon client (hub, hub-app) renders the same theme.
import '@avalon-initiative/common-ui/tokens.css'
import '@avalon-initiative/common-ui/global.css'
import '@avalon-initiative/common-ui/style.css'
import App from './App.vue'
import router from './router'
import { createSecureSessionStorage } from './storage/secureStorage'
import { setUpDeepLinkHandling } from './deepLink'

const app = createApp(App)
const pinia = createPinia()
setActivePinia(pinia)
app.use(pinia)

// Inside Tauri the session token goes through the OS keychain
// (src-tauri/src/lib.rs's native commands), never `localStorage`; configured
// once, before the session store reads anything. A plain browser build (dev
// server, headless checks) has no native commands and keeps the localStorage
// default.
if ('__TAURI_INTERNALS__' in window) {
  configureSessionStorage(createSecureSessionStorage())
}

useSessionStore()
  .initialize()
  .then(() => {
    app.use(router).mount('#app')
    // Epic #623, issue #640 — see deepLink.ts's own doc comment. Fired
    // after mount so a cold-launch deep link's route push lands on a
    // real, already-mounted router.
    void setUpDeepLinkHandling(router)
  })
