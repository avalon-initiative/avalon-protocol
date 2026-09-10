<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// Issue #270's own hard invariant, structurally enforced here rather than
// left to every call site to remember: the numeric value is never rendered
// without its class label and definition alongside it. No composite score,
// no ranking — this tile shows exactly what the registry endpoint
// returned (#261), nothing derived or re-weighted.
import styles from '../styles/AvalonMetricTile.module.scss'
import type { AvalonMetricTileProps } from './AvalonMetricTile.types'

defineProps<AvalonMetricTileProps>()
</script>

<template>
  <div :class="styles.tile">
    <span :class="styles.value">{{ value.toLocaleString() }}</span>
    <span :class="styles.label">{{ label }}</span>
    <span :class="styles.metricClass">{{ metricClass }}</span>
    <p :class="styles.definition">{{ definition }}</p>
  </div>
</template>
