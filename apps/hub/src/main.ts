import { createApp } from 'vue'
import { createPinia, setActivePinia } from 'pinia'
import { useSessionStore } from '@avalon/api-client'
// Design tokens + base page styles come from the shared component library
// so every Avalon client (hub, mobile-hub) renders the same theme.
import '@avalon/ui/src/styles/tokens.css'
import '@avalon/ui/src/styles/global.css'
import App from './App.vue'
import router from './router'

const app = createApp(App)
const pinia = createPinia()
setActivePinia(pinia)
app.use(pinia)

// Issue #60 made session storage pluggable (localStorage here, platform
// secure storage in mobile-hub) and therefore async — hydrate before
// mounting so the router's very first navigation guard
// (`useSessionStore().isAuthenticated()`) sees final state, same as the
// old synchronous `ref(localStorage.getItem(...))` did before this changed.
useSessionStore()
  .initialize()
  .then(() => {
    app.use(router).mount('#app')
  })
