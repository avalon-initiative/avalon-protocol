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
