<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// A generic centered-card layout — knows nothing about WebAuthn, passwords,
// or any specific credential flow (see issue #55's design notes on why this
// stays a plain layout primitive rather than a "swap the credential step"
// abstraction).
import styles from '../styles/AvalonAuthCard.module.scss'
import type { AvalonAuthCardProps } from './AvalonAuthCard.types'

defineProps<AvalonAuthCardProps>()
</script>

<template>
  <div :class="styles.card">
    <div :class="styles.inner">
      <h1 :class="styles.title">{{ title }}</h1>
      <p v-if="subtitle" :class="styles.subtitle">{{ subtitle }}</p>
      <div :class="styles.body">
        <slot />
      </div>
    </div>
  </div>
</template>
