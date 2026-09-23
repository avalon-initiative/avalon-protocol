<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// Filled hexagonal tier badge, separate from AvalonIcon's single-color
// stroke model: the frame carries the tier color, the glyph sits inside.
import styles from '../styles/AvalonBadgeIcon.module.scss'
import AvalonIcon from './AvalonIcon.vue'
import { BADGE_GLYPHS } from '../state/AvalonBadgeIcon.state'
import type { AvalonBadgeIconProps } from '../types/AvalonBadgeIcon.types'

withDefaults(defineProps<AvalonBadgeIconProps>(), {
  size: 48,
})
</script>

<template>
  <span
    :class="[styles.badge, styles[tier]]"
    :style="{ width: `${size}px`, height: `${size}px` }"
    role="img"
    :aria-label="tier"
  >
    <svg :class="styles.frame" viewBox="0 0 48 48" aria-hidden="true">
      <polygon :class="styles.outer" points="24,2 43,13 43,35 24,46 5,35 5,13" />
      <polygon :class="styles.inner" points="24,7 38.5,15.5 38.5,32.5 24,41 9.5,32.5 9.5,15.5" />
    </svg>
    <AvalonIcon :class="styles.glyph" :name="BADGE_GLYPHS[tier]" :size="Math.round(size * 0.46)" />
  </span>
</template>
