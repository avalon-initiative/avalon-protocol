<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// No route awareness here (issue #130's version of the #18 invariant: no
// component in packages/ui knows about tokens, fetches, or routes) — the
// caller determines which tab is active from its own route and passes it
// in via each tab's `active` flag; this component only emits which tab was
// picked, the caller decides what that means (e.g. router.push).
import styles from '../styles/AvalonTabs.module.scss'
import type { AvalonTabsProps } from './AvalonTabs.types'

defineProps<AvalonTabsProps>()
defineEmits<{ select: [to: string] }>()
</script>

<template>
  <nav :class="styles.tabs">
    <button
      v-for="tab in tabs"
      :key="tab.to"
      type="button"
      :class="[styles.tab, tab.active ? styles.active : '']"
      @click="$emit('select', tab.to)"
    >
      {{ tab.label }}
    </button>
  </nav>
</template>
