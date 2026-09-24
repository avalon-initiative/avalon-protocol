import { createRouter, createWebHistory } from 'vue-router'
import { useSessionStore } from '../api/session'

// Small, standalone router — mirrors apps/hub's shape for the auth screens
// but doesn't carry the rest of hub's route tree; guild/friends/
// chat views are separate, later work.
const router = createRouter({
  history: createWebHistory(),
  routes: [
    {
      path: '/',
      name: 'landing',
      component: () => import('../views/Login.vue'),
      beforeEnter: () => (useSessionStore().isAuthenticated() ? '/home' : true),
    },
    { path: '/login', name: 'login', component: () => import('../views/Login.vue') },
    {
      path: '/create-identity',
      name: 'create-identity',
      component: () => import('../views/CreateIdentity.vue'),
    },
    {
      path: '/home',
      name: 'home',
      component: () => import('../views/Home.vue'),
      meta: { requiresAuth: true },
    },
    // Reachable whether logged in or not — a fresh install needs to be able
    // to point at a non-default server before an identity even exists.
    { path: '/settings', name: 'settings', component: () => import('../views/Settings.vue') },
    // Cross-node login approval, reached via a
    // `?node=...&user_code=...` deep link (see `src-tauri/src/lib.rs`'s
    // deep-link listener) or typed in by hand — same shape
    // `apps/hub/src/views/CrossNodeLogin.vue` already established.
    {
      path: '/cross-node-login',
      name: 'cross-node-login',
      component: () => import('../views/CrossNodeLogin.vue'),
      meta: { requiresAuth: true },
    },
  ],
})

router.beforeEach((to) => {
  if (to.meta.requiresAuth && !useSessionStore().isAuthenticated()) {
    return { name: 'login' }
  }
  return true
})

export default router
