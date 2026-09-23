<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// A persistent, non-dismissible notice — deliberately has no
// close/dismiss affordance, unlike a toast: it's meant to keep showing for
// as long as the condition it describes (e.g. "only one passkey
// registered") is true, not just until someone acknowledges it once.
import styles from '../styles/AvalonWarningBanner.module.scss'
import AvalonIcon from './AvalonIcon.vue'
import type { AvalonWarningBannerProps } from '../types/AvalonWarningBanner.types'

withDefaults(defineProps<AvalonWarningBannerProps>(), {
  tone: 'warning',
})
</script>

<template>
  <div :class="[styles.banner, tone === 'danger' ? styles.danger : styles.warning]" role="alert">
    <AvalonIcon name="alert" :class="styles.icon" />
    <div :class="styles.text">
      <p :class="styles.title">{{ title }}</p>
      <p :class="styles.message">{{ message }}</p>
    </div>
  </div>
</template>
