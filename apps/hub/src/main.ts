import { createApp } from 'vue'
import { createPinia, setActivePinia } from 'pinia'
import { useSessionStore } from './api/session'
// Design tokens + base page styles come from the shared component library
// so every Avalon client (hub, hub-app) renders the same theme.
import '@avalon-initiative/common-ui/tokens.css'
import '@avalon-initiative/common-ui/global.css'
import '@avalon-initiative/common-ui/style.css'
import App from './App.vue'
import router from './router'

const app = createApp(App)
const pinia = createPinia()
setActivePinia(pinia)
app.use(pinia)

// initialize() does a real GET /me round trip to build the AccountSession
// — await it before mounting so the router's very first
// navigation guard (`useSessionStore().isAuthenticated()`) sees final
// state, not a still-loading one.
useSessionStore()
  .initialize()
  .then(() => {
    app.use(router).mount('#app')
  })
