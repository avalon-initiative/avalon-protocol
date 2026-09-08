<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// A clickable summary card for "my guilds" (issue #24) — the caller
// decides what a click does (usually a route push); this component knows
// nothing about routing.
import styles from '../styles/AvalonGuildCard.module.scss'
import type { AvalonGuildCardProps } from './AvalonGuildCard.types'

defineProps<AvalonGuildCardProps>()
defineEmits<{ select: [] }>()
</script>

<template>
  <button :class="styles.card" type="button" @click="$emit('select')">
    <div :class="styles.heading">
      <span :class="styles.name">{{ name }}</span>
      <span :class="styles.tag">[{{ tag }}]</span>
    </div>
    <p v-if="description" :class="styles.description">{{ description }}</p>
    <span :class="styles.memberCount">{{ memberCount }} member{{ memberCount === 1 ? '' : 's' }}</span>
  </button>
</template>
