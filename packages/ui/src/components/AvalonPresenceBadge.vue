<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
import styles from '../styles/AvalonPresenceBadge.module.scss'
import { PRESENCE_STATUS_LABELS } from '../types/AvalonPresenceBadge.types'
import type { AvalonPresenceBadgeProps } from '../types/AvalonPresenceBadge.types'

const props = defineProps<AvalonPresenceBadgeProps>()
</script>

<template>
  <span :class="styles.badge">
    <span :class="[styles.dot, styles[status]]" />
    {{ PRESENCE_STATUS_LABELS[props.status] }}
  </span>
</template>
