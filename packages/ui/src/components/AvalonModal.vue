<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// A generic backdrop + panel + close button, content via the default slot,
// action buttons via the optional #actions slot — the caller owns its own
// form/fields and submit/cancel logic entirely.
import styles from '../styles/AvalonModal.module.scss'
import type { AvalonModalProps } from './AvalonModal.types'

defineProps<AvalonModalProps>()
defineEmits<{ close: [] }>()
</script>

<template>
  <div v-if="open" :class="styles.backdrop" @click.self="$emit('close')">
    <div :class="styles.panel" role="dialog" aria-modal="true" :aria-label="title">
      <div :class="styles.header">
        <h2 :class="styles.title">{{ title }}</h2>
        <button type="button" :class="styles.closeButton" aria-label="Close" @click="$emit('close')">
          &times;
        </button>
      </div>
      <div :class="styles.body">
        <slot />
      </div>
      <div v-if="$slots.actions" :class="styles.actions">
        <slot name="actions" />
      </div>
    </div>
  </div>
</template>
