import { createRouter, createWebHistory } from 'vue-router'
import { useSessionStore } from '../stores/session'

const router = createRouter({
  history: createWebHistory(),
  routes: [
    {
      path: '/',
      redirect: () => (useSessionStore().isAuthenticated() ? '/profile' : '/create-identity'),
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
    // The logged-in shell (issue #130) — its own path never resolves
    // directly (there's no exact-'/' record here, that's the redirect
    // above); children below are what's actually reachable
    // ('/profile', '/friends'), rendered inside HubShell.vue's persistent
    // identity header + tab nav rather than as standalone pages.
    // `requiresAuth` here is inherited by every child via vue-router's
    // meta-merging across matched records — no need to repeat it below.
    {
      path: '/',
      component: () => import('../views/HubShell.vue'),
      meta: { requiresAuth: true },
      children: [
        { path: 'profile', name: 'profile', component: () => import('../views/Profile.vue') },
        { path: 'friends', name: 'friends', component: () => import('../views/Friends.vue') },
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
