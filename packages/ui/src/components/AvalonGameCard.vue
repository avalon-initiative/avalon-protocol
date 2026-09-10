<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// A clickable summary card for the game directory (issue #270) — the
// caller decides what a click does (usually a route push to the game's
// profile page); this component knows nothing about routing. Formatting
// (e.g. a relative/localized registeredAt) is the caller's job, same
// "props in, ready-to-render" convention AvalonGuildCard/
// AvalonConnectionCard already use.
import { computed } from 'vue'
import styles from '../styles/AvalonGameCard.module.scss'
import type { AvalonGameCardProps } from './AvalonGameCard.types'

const props = defineProps<AvalonGameCardProps>()
defineEmits<{ select: [] }>()

const isActive = computed(() => props.status === 'active')
</script>

<template>
  <button :class="styles.card" type="button" @click="$emit('select')">
    <div :class="styles.heading">
      <span :class="styles.name">{{ name }}</span>
      <span v-if="!isActive" :class="styles.statusBadge">{{ status }}</span>
    </div>
    <p :class="styles.developer">{{ developer }}</p>
    <span :class="styles.registeredAt">Registered {{ registeredAt }}</span>
  </button>
</template>
