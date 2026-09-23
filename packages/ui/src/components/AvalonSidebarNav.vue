<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// No route awareness — the caller decides which item is active and what a
// `select` means.
// A disabled item never emits: it's a roadmap marker, not a link.
import AvalonIcon from './AvalonIcon.vue'
import styles from '../styles/AvalonSidebarNav.module.scss'
import type { AvalonNavProps } from '../types/AvalonNav.types'

defineProps<AvalonNavProps>()
defineEmits<{ select: [to: string] }>()
</script>

<template>
  <nav :class="styles.nav" aria-label="Primary">
    <button
      v-for="item in items"
      :key="item.to"
      type="button"
      :class="[styles.item, item.active ? styles.active : '', item.disabled ? styles.disabled : '']"
      :disabled="item.disabled"
      :aria-current="item.active ? 'page' : undefined"
      @click="!item.disabled && $emit('select', item.to)"
    >
      <AvalonIcon :name="item.icon" :size="18" />
      <span :class="styles.label">{{ item.label }}</span>
      <span v-if="item.disabled" :class="styles.soon">Soon</span>
    </button>
  </nav>
</template>
