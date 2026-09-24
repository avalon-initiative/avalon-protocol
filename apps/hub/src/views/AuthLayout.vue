<script setup lang="ts">
// The pre-authentication page frame: wordmark above a
// centered card on the dark background. App-level layout, not a library
// component — it's only ever used by Login and CreateIdentity.
import { AvalonIcon } from '@avalon-initiative/common-ui'
import styles from '../styles/AuthLayout.module.scss'
</script>

<template>
  <div :class="styles.layout">
    <div :class="styles.brand">
      <span :class="styles.brandMark"><AvalonIcon name="logo" :size="28" /></span>
      <span :class="styles.brandName">AVALON</span>
    </div>
    <slot />
  </div>
</template>
