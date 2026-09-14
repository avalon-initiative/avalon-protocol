import { createRouter, createWebHistory } from 'vue-router'
import { useSessionStore } from '../stores/session'

const router = createRouter({
  history: createWebHistory(),
  routes: [
    {
      path: '/',
      name: 'landing',
      component: () => import('../views/Landing.vue'),
      beforeEnter: () => (useSessionStore().isAuthenticated() ? '/home' : true),
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
        // Issue #393: a read-only profile card for another identity,
        // reachable from a friend row or a guild member row.
        { path: 'users/:id', name: 'user-profile', component: () => import('../views/UserProfile.vue') },
        { path: 'activity', name: 'activity', component: () => import('../views/Activity.vue') },
        {
          path: 'achievements',
          name: 'achievements',
          component: () => import('../views/Achievements.vue'),
        },
        // #282: generalized off /games; old paths redirect below. #273: public/unauthenticated,
        // so requiresAuth: false overrides HubShell's inherited requiresAuth: true.
        {
          path: 'integrations',
          name: 'integrations',
          component: () => import('../views/GameDirectory.vue'),
          meta: { requiresAuth: false },
        },
        {
          path: 'integrations/:slug',
          name: 'integration-profile',
          component: () => import('../views/GameProfile.vue'),
          meta: { requiresAuth: false },
        },
        { path: 'games', redirect: { name: 'integrations' } },
        { path: 'games/:slug', redirect: (to) => ({ name: 'integration-profile', params: to.params }) },
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
        // Issue #105: same "persistent sidebar, no remount on selection"
        // shape #241 set for guild channels — `conversation` renders the
        // same component as `messages`, the :id param just pre-selects a
        // conversation.
        { path: 'messages', name: 'messages', component: () => import('../views/Messages.vue') },
        {
          path: 'messages/:id',
          name: 'conversation',
          component: () => import('../views/Messages.vue'),
        },
        { path: 'connect/:slug', name: 'connect-game', component: () => import('../views/ConnectGame.vue') },
        { path: 'connections', name: 'connections', component: () => import('../views/Connections.vue') },
        // #307: cross-device pairing — matches `verification_uri`'s
        // `/pair?user_code=...` shape (see crates/server/src/device_pairing.rs).
        { path: 'pair', name: 'pair-device', component: () => import('../views/PairDevice.vue') },
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
