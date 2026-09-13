<script setup lang="ts">
// The logged-out marketing/landing page (issue-less: see NEXT_TASKS). Sits
// in front of Login/CreateIdentity for a visitor who isn't authenticated
// yet — router/index.ts sends unauthenticated `/` here instead of
// redirecting straight into the create-identity flow.
import { AvalonButton, AvalonCard, AvalonIcon } from '@avalon/ui'
import styles from './Landing.module.scss'

const audiences = [
  {
    icon: 'friends' as const,
    label: 'For players',
    body: 'Bring your identity, friends, and guilds with you. Log in the same way everywhere, and keep the people and history you’ve built even after a game moves on.',
  },
  {
    icon: 'library' as const,
    label: 'For developers',
    body: 'Plug your game into an identity, social, and achievement layer you didn’t have to build yourself — without handing over control of your world.',
  },
  {
    icon: 'community' as const,
    label: 'For communities',
    body: 'Give your guild a home that outlasts any one game — membership, roles, and chat that travel with the community, not with a server that could disappear.',
  },
  {
    icon: 'settings' as const,
    label: 'For operators',
    body: 'Run and support the infrastructure the network settles on — nodes that keep the shared history durable and verifiable for everyone connected to it.',
  },
]
</script>

<template>
  <div :class="styles.page">
    <header :class="styles.nav">
      <div :class="styles.brand">
        <span :class="styles.brandMark"><AvalonIcon name="logo" :size="24" /></span>
        <span :class="styles.brandName">AVALON</span>
      </div>
      <div :class="styles.navActions">
        <RouterLink to="/login" :class="styles.navLink">Log in</RouterLink>
        <RouterLink to="/create-identity"><AvalonButton label="Create identity" /></RouterLink>
      </div>
    </header>

    <section :class="styles.hero">
      <h1 :class="styles.headline">One identity. Every world you play in.</h1>
      <p :class="styles.subhead">
        Avalon is an open network for players, games, and communities. Your identity, friends, guilds,
        and achievements don’t belong to any single game — they move with you, so a world can end
        without taking your community down with it.
      </p>
      <div :class="styles.heroActions">
        <RouterLink to="/create-identity"><AvalonButton label="Create your identity" /></RouterLink>
        <RouterLink to="/login" :class="styles.secondaryLink">Already have an identity? Log in</RouterLink>
      </div>
    </section>

    <section :class="styles.audiences">
      <AvalonCard v-for="a in audiences" :key="a.label" :class="styles.audienceCard">
        <div :class="styles.audienceIcon"><AvalonIcon :name="a.icon" :size="22" /></div>
        <h3 :class="styles.audienceLabel">{{ a.label }}</h3>
        <p :class="styles.audienceBody">{{ a.body }}</p>
      </AvalonCard>
    </section>

    <footer :class="styles.footer">
      <p>One network. Many worlds.</p>
    </footer>
  </div>
</template>
