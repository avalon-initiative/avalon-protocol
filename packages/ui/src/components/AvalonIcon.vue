<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// A tiny inline-SVG icon set — deliberately no icon-library dependency
// every glyph is a 24x24 stroke path using currentColor, so
// it takes the color of whatever text it sits in.
import styles from '../styles/AvalonIcon.module.scss'
import type { AvalonIconProps } from '../types/AvalonIcon.types'

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
    <template v-else-if="name === 'integrators'">
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
      <!-- The real mark (#311, `.vscode/mocks/IconMock.png`'s "Monochrome"
           app-icon variant), replacing this glyph's pre-mock placeholder
           shape - a solid diamond silhouette, filled rather than stroked
           like every other icon here, matching the faceted gem used in
           apps/mobile-hub/src-tauri/app-icon.svg and apps/hub/public/favicon.svg. -->
      <path d="M12 2 20.5 9 12 22 3.5 9 12 2Z" fill="currentColor" stroke="none" />
    </template>
    <template v-else-if="name === 'alert'">
      <path d="M12 3.5 2 20.5h20L12 3.5Z" />
      <path d="M12 10v4.5" />
      <circle cx="12" cy="17.5" r="0.9" fill="currentColor" stroke="none" />
    </template>
    <template v-else-if="name === 'pencil'">
      <path d="M4 20l0.9-4.2L15.5 5.2a1.5 1.5 0 0 1 2.1 0l1.2 1.2a1.5 1.5 0 0 1 0 2.1L8.2 19.1 4 20Z" />
      <path d="M13.8 6.9l3.3 3.3" />
    </template>
    <template v-else-if="name === 'check'">
      <path d="M4.5 12.5 9.5 17.5 19.5 6.5" />
    </template>
    <template v-else-if="name === 'close'">
      <path d="M5.5 5.5 18.5 18.5M18.5 5.5 5.5 18.5" />
    </template>
    <template v-else-if="name === 'settings'">
      <circle cx="12" cy="12" r="3" />
      <path
        d="M12 3.5v2.2M12 18.3v2.2M20.5 12h-2.2M5.7 12H3.5M17.8 6.2l-1.5 1.5M7.7 16.3l-1.5 1.5M17.8 17.8l-1.5-1.5M7.7 7.7 6.2 6.2"
      />
    </template>
    <template v-else-if="name === 'voice'">
      <rect x="9" y="3" width="6" height="11" rx="3" />
      <path d="M5.5 11.5A6.5 6.5 0 0 0 12 18a6.5 6.5 0 0 0 6.5-6.5" />
      <path d="M12 18v3" />
    </template>
    <template v-else-if="name === 'video'">
      <rect x="3" y="6.5" width="12" height="11" rx="2" />
      <path d="M15 10.5 21 7v10l-6-3.5Z" />
    </template>
    <template v-else-if="name === 'messages'">
      <rect x="3" y="5.5" width="18" height="13" rx="2" />
      <path d="m4 7 8 6 8-6" />
    </template>
    <template v-else-if="name === 'calendar'">
      <rect x="3.5" y="5" width="17" height="15" rx="2" />
      <path d="M3.5 9.5h17M8 3v3.5M16 3v3.5" />
    </template>
    <template v-else-if="name === 'achievements'">
      <path d="M8 4h8v5a4 4 0 0 1-8 0V4Z" />
      <path d="M8 5.5H4.5v1.5A3 3 0 0 0 8 9.5M16 5.5h3.5V7A3 3 0 0 1 16 9.5" />
      <path d="M12 13v3.5M9 20.5h6M10.2 16.5h3.6l0.4 4h-4.4l0.4-4Z" />
    </template>
    <template v-else-if="name === 'library'">
      <path d="M5 4.5h9v16H5a1.5 1.5 0 0 1-1.5-1.5v-13A1.5 1.5 0 0 1 5 4.5Z" />
      <path d="M14 6l4.6 1.2-3.3 12.7L11 18.7" />
    </template>
    <template v-else-if="name === 'wallet'">
      <path d="M4 7.5A2.5 2.5 0 0 1 6.5 5h11A2.5 2.5 0 0 1 20 7.5v9a2.5 2.5 0 0 1-2.5 2.5h-11A2.5 2.5 0 0 1 4 16.5v-9Z" />
      <path d="M14.5 12.5h4v3h-4a1.5 1.5 0 0 1 0-3Z" />
    </template>
    <template v-else-if="name === 'more'">
      <circle cx="5.5" cy="12" r="1.4" fill="currentColor" stroke="none" />
      <circle cx="12" cy="12" r="1.4" fill="currentColor" stroke="none" />
      <circle cx="18.5" cy="12" r="1.4" fill="currentColor" stroke="none" />
    </template>
    <template v-else-if="name === 'community'">
      <circle cx="8" cy="8.5" r="3" />
      <circle cx="16" cy="8.5" r="3" />
      <path d="M2.5 20c0-3.3 2.5-5.5 5.5-5.5s5.5 2.2 5.5 5.5M10.5 20c0-3.3 2.5-5.5 5.5-5.5s5.5 2.2 5.5 5.5" />
    </template>
    <template v-else-if="name === 'faction'">
      <path d="M5 4.5h11l-1.6 3.5L16 11.5H5Z" />
      <path d="M5 4.5v15" />
    </template>
    <template v-else-if="name === 'event'">
      <rect x="3.5" y="5" width="17" height="15" rx="2" />
      <path d="M3.5 9.5h17M8 3v3.5M16 3v3.5" />
      <path d="m9 14.5 2 2 4-4.5" />
    </template>
    <template v-else-if="name === 'reward'">
      <path d="M7 4h10v5a5 5 0 0 1-10 0V4Z" />
      <path d="M7 5.5H4a3 3 0 0 0 3 4M17 5.5h3a3 3 0 0 1-3 4" />
      <path d="M12 14v3M9.5 20.5h5" />
    </template>
    <template v-else-if="name === 'leaderboards'">
      <path d="M5 20.5V13M11 20.5V8M17 20.5v-6" />
      <path d="M3.5 20.5h17" />
    </template>
    <template v-else-if="name === 'map'">
      <path d="M9 4.5 15 6.5 21 4.5v14L15 20.5 9 18.5 3 20.5v-14Z" />
      <path d="M9 4.5v14M15 6.5v14" />
    </template>
    <template v-else-if="name === 'join'">
      <path d="M3.5 12h11M10.5 7l4 5-4 5" />
      <path d="M18.5 5v14" />
    </template>
    <template v-else-if="name === 'leave'">
      <path d="M20.5 12h-11M13.5 7l-4 5 4 5" />
      <path d="M5.5 5v14" />
    </template>
    <template v-else-if="name === 'invite'">
      <circle cx="9" cy="8.5" r="3.5" />
      <path d="M2.5 20c0-3.6 2.9-6 6.5-6s6.5 2.4 6.5 6" />
      <path d="M17.5 8.5v5M15 11h5" />
    </template>
    <template v-else-if="name === 'share'">
      <circle cx="18" cy="6" r="2.2" />
      <circle cx="6" cy="12" r="2.2" />
      <circle cx="18" cy="18" r="2.2" />
      <path d="m8 10.8 8-3.6M8 13.2l8 3.6" />
    </template>
    <template v-else-if="name === 'bookmark'">
      <path d="M6 4h12v16l-6-4-6 4Z" />
    </template>
    <template v-else-if="name === 'follow'">
      <path
        d="M12 20.2s-7.5-4.4-7.5-10a4.4 4.4 0 0 1 7.5-3.1A4.4 4.4 0 0 1 19.5 10.2c0 5.6-7.5 10-7.5 10Z"
      />
    </template>
    <template v-else-if="name === 'muted'">
      <rect x="9" y="3" width="6" height="11" rx="3" />
      <path d="M5.5 11.5A6.5 6.5 0 0 0 12 18a6.5 6.5 0 0 0 6.5-6.5" />
      <path d="M12 18v3M3 3l18 18" />
    </template>
    <template v-else-if="name === 'block'">
      <circle cx="12" cy="12" r="9" />
      <path d="m5.6 5.6 12.8 12.8" />
    </template>
    <template v-else-if="name === 'trophy'">
      <path d="M8 4h8v5a4 4 0 0 1-8 0V4Z" />
      <path d="M8 5.5H4.5v1.5A3 3 0 0 0 8 9.5M16 5.5h3.5V7A3 3 0 0 1 16 9.5" />
      <path d="M12 13v3.5M9 20.5h6M10.2 16.5h3.6l0.4 4h-4.4l0.4-4Z" />
    </template>
    <template v-else-if="name === 'star'">
      <path d="M12 3.5 14.6 9.2 20.8 9.9 16.2 14 17.5 20.2 12 17 6.5 20.2 7.8 14 3.2 9.9 9.4 9.2Z" />
    </template>
    <template v-else-if="name === 'shield'">
      <path d="M12 3 4.5 6v5.5c0 4.6 3.2 8 7.5 9.5 4.3-1.5 7.5-4.9 7.5-9.5V6L12 3Z" />
    </template>
    <template v-else-if="name === 'sword'">
      <path d="m6 18 9.5-9.5M13.5 4.5l6 6-2.5 2.5-6-6Z" />
      <path d="M4 20l2.5-1 0.5-2.5-2 0.5Z" />
    </template>
    <template v-else-if="name === 'crown'">
      <path d="M4 9.5 8 12l4-6 4 6 4-2.5-1.5 8.5h-13Z" />
      <path d="M6 18.5h12" />
    </template>
    <template v-else-if="name === 'wrench'">
      <path d="M14.5 6.5a4 4 0 0 1-5.4 5.4L4 17l3 3 5.1-5.1a4 4 0 0 1 5.4-5.4l-2.6 2.6-2-2Z" />
    </template>
    <template v-else-if="name === 'heart'">
      <path d="M12 20.5S4 15.8 4 9.8a4.2 4.2 0 0 1 8-1.8 4.2 4.2 0 0 1 8 1.8c0 6-8 10.7-8 10.7Z" />
    </template>
    <template v-else-if="name === 'flag'">
      <path d="M6 3v18" />
      <path d="M6 4.5h11l-2.5 3.5L17 11.5H6Z" />
    </template>
    <template v-else-if="name === 'bolt'">
      <path d="M13 3 5 13.5h5.5L11 21l8-10.5h-5.5Z" />
    </template>
  </svg>
</template>
