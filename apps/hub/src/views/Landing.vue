<script setup lang="ts">
// The logged-out marketing/landing page (issue-less: see NEXT_TASKS). Sits
// in front of Login/CreateIdentity for a visitor who isn't authenticated
// yet — router/index.ts sends unauthenticated `/` here instead of
// redirecting straight into the create-identity flow.
import { AvalonButton, AvalonCard, AvalonIcon } from '@avalon-initiative/common-ui'
import styles from '../styles/Landing.module.scss'

const audiences = [
  {
    icon: 'friends' as const,
    accent: 'primary' as const,
    label: 'For users',
    body: 'Bring your identity, friends, and guilds with you. Log in the same way everywhere, and keep the people and history you’ve built even after an integrator moves on.',
  },
  {
    icon: 'library' as const,
    accent: 'secondary' as const,
    label: 'For developers',
    body: 'Plug your integrator into an identity, social, and achievement layer you didn’t have to build yourself — without handing over control of your world.',
  },
  {
    icon: 'community' as const,
    accent: 'tertiary' as const,
    label: 'For communities',
    body: 'Give your guild a home that outlasts any one integrator — membership, roles, and chat that travel with the community, not with a server that could disappear.',
  },
  {
    icon: 'settings' as const,
    accent: 'primary' as const,
    label: 'For operators',
    body: 'Run and support the infrastructure the network settles on — nodes that keep the shared history durable and verifiable for everyone connected to it.',
  },
]

const whatIsThis = [
  {
    icon: 'discover' as const,
    kicker: 'network',
    label: 'An open protocol',
    body: 'Avalon is the connective layer — identity, friends and guilds, achievements — not a platform that owns your world. Your integrator stays sovereign over its rules, economy, and world state; Avalon is the shared layer underneath it, the way OAuth or Steam Cloud sit under an app without being the app.',
    snippet: 'integrator.connect(avalon)',
  },
  {
    icon: 'device' as const,
    kicker: 'infrastructure',
    label: 'Self-hosted, your call',
    body: 'Run the reference server yourself — as more infrastructure mirroring the shared public network, or as a fully private, disconnected instance rooted under your own network id for internal use, dev, or air-gapped environments. The two are cryptographically incapable of being confused with each other.',
    snippet: 'AVALON_NETWORK_ID=your-own',
  },
]

const steps = [
  {
    icon: 'profile' as const,
    label: 'Create your identity',
    body: 'One self-custodied keypair — a passkey to log in, a signing key that’s yours alone.',
  },
  {
    icon: 'integrators' as const,
    label: 'Connect it everywhere',
    body: 'Any integrator or app on the network recognizes the same you, with your consent.',
  },
  {
    icon: 'achievements' as const,
    label: 'Keep what you build',
    body: 'Friends, guilds, and achievements persist — even if a world shuts down.',
  },
]

const GITHUB_PROFILE_URL = 'https://github.com/avalon-initiative'
</script>

<template>
  <div :class="styles.page">
    <div :class="styles.bgGlow" aria-hidden="true" />

    <header :class="styles.nav">
      <div :class="styles.brand">
        <span :class="styles.brandMark"><AvalonIcon name="logo" :size="22" /></span>
        <span :class="styles.brandName">AVALON</span>
      </div>
      <div :class="styles.navActions">
        <RouterLink to="/login" :class="styles.navLink">Log in</RouterLink>
        <RouterLink to="/create-identity"><AvalonButton label="Create identity" /></RouterLink>
      </div>
    </header>

    <section :class="styles.hero">
      <div :class="styles.heroText">
        <span :class="styles.eyebrow">An open network for users, integrators, and communities</span>
        <h1 :class="styles.headline">One identity.<br />Every world you play in.</h1>
        <p :class="styles.subhead">
          Your identity, friends, guilds, and achievements don’t belong to any single integrator —
          they move with you, so a world can end without taking your community down with it.
        </p>
        <div :class="styles.heroActions">
          <RouterLink to="/create-identity"><AvalonButton label="Create your identity" /></RouterLink>
          <RouterLink to="/login" :class="styles.secondaryLink">Already have an identity? Log in</RouterLink>
        </div>
      </div>

      <div :class="styles.heroVisual" aria-hidden="true">
        <svg
          :class="styles.constellation"
          viewBox="0 0 600 600"
          fill="none"
          xmlns="http://www.w3.org/2000/svg"
        >
          <g :class="styles.edges" stroke-width="1.4">
            <line x1="300" y1="300" x2="150" y2="120" />
            <line x1="300" y1="300" x2="420" y2="90" />
            <line x1="300" y1="300" x2="480" y2="260" />
            <line x1="300" y1="300" x2="380" y2="420" />
            <line x1="300" y1="300" x2="180" y2="430" />
            <line x1="300" y1="300" x2="90" y2="280" />
            <line x1="150" y1="120" x2="260" y2="160" />
            <line x1="420" y1="90" x2="350" y2="200" />
            <line x1="480" y1="260" x2="420" y2="340" />
            <line x1="380" y1="420" x2="240" y2="380" />
            <line x1="180" y1="430" x2="60" y2="400" />
            <line x1="90" y1="280" x2="130" y2="200" />
            <line x1="260" y1="160" x2="350" y2="200" />
            <line x1="130" y1="200" x2="150" y2="120" />
            <line x1="520" y1="150" x2="420" y2="90" />
          </g>
          <g :class="styles.nodes">
            <circle :class="styles.nodePrimary" cx="300" cy="300" r="11" style="--i: 0" />
            <circle :class="styles.nodeSecondary" cx="150" cy="120" r="6" style="--i: 1" />
            <circle :class="styles.nodeTertiary" cx="420" cy="90" r="5" style="--i: 2" />
            <circle :class="styles.nodePrimary" cx="480" cy="260" r="7" style="--i: 3" />
            <circle :class="styles.nodeSecondary" cx="380" cy="420" r="6" style="--i: 4" />
            <circle :class="styles.nodeTertiary" cx="180" cy="430" r="5" style="--i: 5" />
            <circle :class="styles.nodePrimary" cx="90" cy="280" r="6" style="--i: 6" />
            <circle :class="styles.nodeTertiary" cx="260" cy="160" r="4" style="--i: 7" />
            <circle :class="styles.nodeSecondary" cx="350" cy="200" r="4" style="--i: 8" />
            <circle :class="styles.nodePrimary" cx="240" cy="380" r="4" style="--i: 9" />
            <circle :class="styles.nodeTertiary" cx="420" cy="340" r="4" style="--i: 10" />
            <circle :class="styles.nodeSecondary" cx="130" cy="200" r="4" style="--i: 11" />
            <circle :class="styles.nodePrimary" cx="520" cy="150" r="4" style="--i: 12" />
            <circle :class="styles.nodeSecondary" cx="60" cy="400" r="4" style="--i: 13" />
          </g>
        </svg>
      </div>
    </section>

    <section :class="styles.whatIsThis">
      <div :class="styles.whatIsThisIntro">
        <span :class="styles.eyebrow">What is this?</span>
        <h2 :class="styles.whatIsThisHeadline">A protocol you integrate into, not a platform you join.</h2>
      </div>
      <div :class="styles.whatIsThisGrid">
        <article v-for="w in whatIsThis" :key="w.label" :class="styles.whatIsThisCard">
          <div :class="styles.whatIsThisIcon"><AvalonIcon :name="w.icon" :size="22" /></div>
          <span :class="styles.whatIsThisKicker">{{ w.kicker }}</span>
          <h3 :class="styles.whatIsThisLabel">{{ w.label }}</h3>
          <p :class="styles.whatIsThisBody">{{ w.body }}</p>
          <code :class="styles.whatIsThisSnippet">{{ w.snippet }}</code>
        </article>
      </div>
    </section>

    <section :class="styles.steps">
      <div v-for="(s, i) in steps" :key="s.label" :class="styles.step">
        <div :class="styles.stepNumber">{{ i + 1 }}</div>
        <div :class="styles.stepIcon"><AvalonIcon :name="s.icon" :size="20" /></div>
        <h3 :class="styles.stepLabel">{{ s.label }}</h3>
        <p :class="styles.stepBody">{{ s.body }}</p>
      </div>
    </section>

    <section :class="styles.audiences">
      <AvalonCard
        v-for="a in audiences"
        :key="a.label"
        :class="[styles.audienceCard, styles[a.accent]]"
      >
        <div :class="styles.audienceIcon"><AvalonIcon :name="a.icon" :size="22" /></div>
        <h3 :class="styles.audienceLabel">{{ a.label }}</h3>
        <p :class="styles.audienceBody">{{ a.body }}</p>
      </AvalonCard>
    </section>

    <div :class="styles.footerSpacer" aria-hidden="true" />
    <footer :class="styles.footer">
      <div :class="styles.footerBrand">
        <span :class="styles.brandMark"><AvalonIcon name="logo" :size="18" /></span>
        <span :class="styles.brandName">AVALON</span>
      </div>
      <p :class="styles.footerTagline">Infinite worlds. One you.</p>
      <a
        :class="styles.footerLink"
        :href="GITHUB_PROFILE_URL"
        target="_blank"
        rel="noopener noreferrer"
      >
        Avalon Initiative
      </a>
    </footer>
  </div>
</template>
