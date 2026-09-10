import { createRouter, createWebHistory } from 'vue-router'
import { useSessionStore } from '../stores/session'

const router = createRouter({
  history: createWebHistory(),
  routes: [
    {
      path: '/',
      redirect: () => (useSessionStore().isAuthenticated() ? '/home' : '/create-identity'),
    },
    {
      path: '/create-identity',
      name: 'create-identity',
      component: () => import('../views/CreateIdentity.vue'),
    },
    {
      path: '/login',
      name: 'login',
      component: () => import('../views/Login.vue'),
    },
    {
      path: '/recover-identity',
      name: 'recover-identity',
      component: () => import('../views/RecoverIdentity.vue'),
    },
    // The logged-in shell: sidebar (desktop) / bottom nav (mobile) around a
    // <RouterView />, with every page a nested child route so it stays
    // URL-addressable. `requiresAuth` here is inherited by every child via
    // vue-router's meta-merging across matched records.
    {
      path: '/',
      component: () => import('../views/HubShell.vue'),
      meta: { requiresAuth: true },
      children: [
        { path: 'home', name: 'home', component: () => import('../views/Home.vue') },
        { path: 'profile', name: 'profile', component: () => import('../views/Profile.vue') },
        { path: 'friends', name: 'friends', component: () => import('../views/Friends.vue') },
        { path: 'activity', name: 'activity', component: () => import('../views/Activity.vue') },
        // Issue #273: GET /games is public/unauthenticated (#270) and this
        // directory is meant to be browsable by a logged-out prospective
        // player. vue-router's meta merging is a shallow Object.assign
        // across `to.matched` (parent then child), so the leaf's
        // `requiresAuth: false` here overrides the HubShell parent's
        // `requiresAuth: true` in the merged `to.meta` the guard below
        // reads — the parent route itself, and every other child, stays
        // gated.
        {
          path: 'games',
          name: 'games',
          component: () => import('../views/GameDirectory.vue'),
          meta: { requiresAuth: false },
        },
        {
          path: 'games/:slug',
          name: 'game-profile',
          component: () => import('../views/GameProfile.vue'),
          meta: { requiresAuth: false },
        },
        { path: 'guilds', name: 'guilds', component: () => import('../views/Guilds.vue') },
        // Issue #241: the Channels tab lives inside Guild.vue itself now
        // (a persistent channel sidebar, no route hop to switch channels),
        // so `guild-channel` renders the same component as `guild` — the
        // :cid param just pre-selects a channel and opens the Channels tab
        // on load, preserving the old deep link.
        { path: 'guilds/:id', name: 'guild', component: () => import('../views/Guild.vue') },
        {
          path: 'guilds/:id/channels/:cid',
          name: 'guild-channel',
          component: () => import('../views/Guild.vue'),
        },
        { path: 'connect/:slug', name: 'connect-game', component: () => import('../views/ConnectGame.vue') },
        { path: 'connections', name: 'connections', component: () => import('../views/Connections.vue') },
      ],
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
