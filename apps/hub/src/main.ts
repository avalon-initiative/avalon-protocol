import { createApp } from 'vue'
import { createPinia } from 'pinia'
// Design tokens + base page styles come from the shared component library
// so every Avalon client (hub, mobile-hub) renders the same theme.
import '@avalon/ui/src/styles/tokens.css'
import '@avalon/ui/src/styles/global.css'
import App from './App.vue'
import router from './router'

createApp(App).use(createPinia()).use(router).mount('#app')
