<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
import styles from '../styles/AvalonPresenceBadge.module.scss'
import type { AvalonPresenceBadgeProps } from './AvalonPresenceBadge.types'

defineProps<AvalonPresenceBadgeProps>()
</script>

<template>
  <span :class="styles.badge">
    <span :class="[styles.dot, styles[status]]" />
    {{ status }}
  </span>
</template>
