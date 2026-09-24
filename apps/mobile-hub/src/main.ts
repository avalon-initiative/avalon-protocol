import { createApp } from 'vue'
import { createPinia, setActivePinia } from 'pinia'
import { configureSessionStorage, useSessionStore } from '@avalon/api-client'
// Design tokens + base page styles come from the shared component library
// so every Avalon client (hub, mobile-hub) renders the same theme.
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

// Issue #60's whole point: the session token goes through the OS keychain
// (src-tauri/src/lib.rs's native command) on this platform, never
// `localStorage` — configured once, before the session store reads
// anything, same as apps/hub leaves this at its `localStorage` default.
configureSessionStorage(createSecureSessionStorage())

useSessionStore()
  .initialize()
  .then(() => {
    app.use(router).mount('#app')
    // Epic #623, issue #640 — see deepLink.ts's own doc comment. Fired
    // after mount so a cold-launch deep link's route push lands on a
    // real, already-mounted router.
    void setUpDeepLinkHandling(router)
  })
