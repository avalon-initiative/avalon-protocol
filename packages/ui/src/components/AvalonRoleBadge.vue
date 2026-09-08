<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
import styles from '../styles/AvalonRoleBadge.module.scss'
import type { AvalonRoleBadgeProps } from './AvalonRoleBadge.types'

withDefaults(defineProps<AvalonRoleBadgeProps>(), {
  variant: 'member',
})
</script>

<template>
  <span :class="[styles.badge, styles[variant]]">{{ name }}</span>
</template>
