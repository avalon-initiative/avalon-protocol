<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// The one surface every dashboard section sits on: optional
// title row with an `action` slot on the right (e.g. a "View all" link),
// then whatever the caller puts in the default slot.
import styles from '../styles/AvalonCard.module.scss'
import type { AvalonCardProps } from '../types/AvalonCard.types'

defineProps<AvalonCardProps>()
</script>

<template>
  <section :class="styles.card">
    <header v-if="title || $slots.action" :class="styles.header">
      <div :class="styles.heading">
        <h2 v-if="title" :class="styles.title">{{ title }}</h2>
        <p v-if="subtitle" :class="styles.subtitle">{{ subtitle }}</p>
      </div>
      <div v-if="$slots.action" :class="styles.action">
        <slot name="action" />
      </div>
    </header>
    <div :class="styles.body">
      <slot />
    </div>
  </section>
</template>
