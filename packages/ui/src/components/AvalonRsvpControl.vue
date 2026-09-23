<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// A three-way RSVP toggle — always the caller's own status,
// never another member's (see .types.ts). Purely presentational: the
// parent owns the actual PUT .../rsvp call and passes the result back
// down as `currentStatus`.
import styles from '../styles/AvalonRsvpControl.module.scss'
import type { AvalonRsvpControlProps, AvalonRsvpStatus } from '../types/AvalonRsvpControl.types'

withDefaults(defineProps<AvalonRsvpControlProps>(), {
  disabled: false,
})
defineEmits<{ rsvp: [status: AvalonRsvpStatus] }>()

const options: { status: AvalonRsvpStatus; label: string }[] = [
  { status: 'going', label: 'Going' },
  { status: 'maybe', label: 'Maybe' },
  { status: 'not_going', label: "Can't go" },
]
</script>

<template>
  <div :class="styles.control" role="group" aria-label="RSVP">
    <button
      v-for="option in options"
      :key="option.status"
      type="button"
      :class="[styles.option, styles[option.status], currentStatus === option.status && styles.active]"
      :disabled="disabled"
      :aria-pressed="currentStatus === option.status"
      @click="$emit('rsvp', option.status)"
    >
      {{ option.label }}
    </button>
  </div>
</template>
