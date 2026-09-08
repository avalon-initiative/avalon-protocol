<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// The mobile counterpart of AvalonSidebarNav — same item shape, same
// "disabled never emits" rule, laid out as a fixed bottom bar.
import AvalonIcon from './AvalonIcon.vue'
import styles from '../styles/AvalonBottomNav.module.scss'
import type { AvalonNavProps } from './AvalonNav.types'

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
      <AvalonIcon :name="item.icon" :size="20" />
      <span :class="styles.label">{{ item.label }}</span>
    </button>
  </nav>
</template>
