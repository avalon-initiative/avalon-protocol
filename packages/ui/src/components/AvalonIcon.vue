<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// A tiny inline-SVG icon set — deliberately no icon-library dependency
// (issue #148); every glyph is a 24x24 stroke path using currentColor, so
// it takes the color of whatever text it sits in.
import styles from '../styles/AvalonIcon.module.scss'
import type { AvalonIconProps } from './AvalonIcon.types'

withDefaults(defineProps<AvalonIconProps>(), {
  size: 20,
})
</script>

<template>
  <svg
    :class="styles.icon"
    :width="size"
    :height="size"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.8"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    <template v-if="name === 'home'">
      <path d="M3 11.5 12 4l9 7.5" />
      <path d="M5 10.5V20h14v-9.5" />
    </template>
    <template v-else-if="name === 'games'">
      <rect x="2.5" y="7" width="19" height="11" rx="4" />
      <path d="M7 11v3M5.5 12.5h3" />
      <circle cx="16" cy="11.5" r="0.9" fill="currentColor" stroke="none" />
      <circle cx="18" cy="13.5" r="0.9" fill="currentColor" stroke="none" />
    </template>
    <template v-else-if="name === 'guilds'">
      <path d="M12 3 4.5 6v5.5c0 4.6 3.2 8 7.5 9.5 4.3-1.5 7.5-4.9 7.5-9.5V6L12 3Z" />
      <path d="M9 12.5l2 2 4-4.5" />
    </template>
    <template v-else-if="name === 'friends'">
      <circle cx="9" cy="8.5" r="3.5" />
      <path d="M2.5 20c0-3.6 2.9-6 6.5-6s6.5 2.4 6.5 6" />
      <circle cx="17" cy="9.5" r="2.5" />
      <path d="M17 14c2.6 0 4.5 1.9 4.5 4.5" />
    </template>
    <template v-else-if="name === 'chat'">
      <path d="M4 5.5h16v10H9l-5 4v-14Z" />
    </template>
    <template v-else-if="name === 'discover'">
      <circle cx="12" cy="12" r="9" />
      <path d="m15.5 8.5-2 5-5 2 2-5 5-2Z" />
    </template>
    <template v-else-if="name === 'profile'">
      <circle cx="12" cy="8" r="4" />
      <path d="M4.5 20.5c0-4 3.4-6.5 7.5-6.5s7.5 2.5 7.5 6.5" />
    </template>
    <template v-else-if="name === 'search'">
      <circle cx="11" cy="11" r="6.5" />
      <path d="m20 20-4.2-4.2" />
    </template>
    <template v-else-if="name === 'bell'">
      <path d="M6 16.5V11a6 6 0 0 1 12 0v5.5l1.5 2h-15l1.5-2Z" />
      <path d="M10 20.5a2 2 0 0 0 4 0" />
    </template>
    <template v-else-if="name === 'plus'">
      <path d="M12 5v14M5 12h14" />
    </template>
    <template v-else-if="name === 'device'">
      <rect x="4" y="3.5" width="16" height="17" rx="2.5" />
      <path d="M10 17.5h4" />
    </template>
    <template v-else-if="name === 'activity'">
      <path d="M3 12h4l2.5-6 4 12 2.5-6H21" />
    </template>
    <template v-else-if="name === 'logo'">
      <path d="M12 2.5 3.5 20h4.2l1.7-3.8h5.2L16.3 20h4.2L12 2.5Z" />
      <path d="M10.6 13.2h2.8L12 9.9l-1.4 3.3Z" fill="currentColor" stroke="none" />
    </template>
    <template v-else-if="name === 'alert'">
      <path d="M12 3.5 2 20.5h20L12 3.5Z" />
      <path d="M12 10v4.5" />
      <circle cx="12" cy="17.5" r="0.9" fill="currentColor" stroke="none" />
    </template>
  </svg>
</template>
