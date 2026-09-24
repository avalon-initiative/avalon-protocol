<script setup lang="ts">
// The pre-authentication page frame — same shape as apps/hub's own
// AuthLayout.vue, sized for the narrower Tauri window instead of a wide
// desktop viewport.
import { AvalonIcon } from '@avalon-initiative/common-ui'
import styles from '../styles/AuthLayout.module.scss'
</script>

<template>
  <div :class="styles.layout">
    <div :class="styles.brand">
      <span :class="styles.brandMark"><AvalonIcon name="logo" :size="24" /></span>
      <span :class="styles.brandName">AVALON</span>
    </div>
    <slot />
  </div>
</template>
