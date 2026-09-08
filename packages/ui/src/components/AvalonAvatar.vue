<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// Renders a real avatar image when a URL is given, otherwise the name's
// first letter on a tinted circle — a caller never has to special-case
// "no avatar yet" (issue #148).
import styles from '../styles/AvalonAvatar.module.scss'
import type { AvalonAvatarProps } from './AvalonAvatar.types'

withDefaults(defineProps<AvalonAvatarProps>(), {
  src: null,
  size: 'md',
})
</script>

<template>
  <span :class="[styles.avatar, styles[size]]" :title="name">
    <img v-if="src" :class="styles.image" :src="src" :alt="name" />
    <span v-else :class="styles.initial" aria-hidden="true">{{ name.trim().charAt(0).toUpperCase() || '?' }}</span>
  </span>
</template>
