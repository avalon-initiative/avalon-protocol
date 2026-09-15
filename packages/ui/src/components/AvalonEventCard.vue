<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// A guild calendar entry (issue #169) — title/time/RSVP counts. The caller
// supplies the RSVP control via the `actions` slot, keeping this component
// itself dumb about mutation (same "props in, click out" split
// AvalonGuildCard/AvalonChannelList already use).
import { computed } from 'vue'
import styles from '../styles/AvalonEventCard.module.scss'
import type { AvalonEventCardProps } from '../types/AvalonEventCard.types'

const props = defineProps<AvalonEventCardProps>()

// Stored/transmitted as UTC (ISO 8601), always displayed converted to the
// viewer's own local timezone — timeZoneName spells that out explicitly
// (e.g. "EDT") rather than leaving members in different timezones to
// guess whether a time is already theirs or needs converting.
// dateStyle/timeStyle can't be combined with timeZoneName (throws), so
// this spells out the equivalent individual fields instead.
const DISPLAY_OPTIONS: Intl.DateTimeFormatOptions = {
  year: 'numeric',
  month: 'short',
  day: 'numeric',
  hour: 'numeric',
  minute: '2-digit',
  timeZoneName: 'short',
}

const timeRange = computed(() => {
  const starts = new Date(props.startsAt).toLocaleString(undefined, DISPLAY_OPTIONS)
  if (!props.endsAt) return starts
  const ends = new Date(props.endsAt).toLocaleString(undefined, DISPLAY_OPTIONS)
  return `${starts} – ${ends}`
})

const totalRsvps = computed(
  () => props.rsvpCounts.going + props.rsvpCounts.maybe + props.rsvpCounts.not_going,
)
</script>

<template>
  <div :class="styles.card">
    <div :class="styles.heading">
      <span :class="styles.title">{{ title }}</span>
      <span :class="styles.time">{{ timeRange }}</span>
    </div>
    <p v-if="description" :class="styles.description">{{ description }}</p>
    <div :class="styles.rsvpSummary">
      <span :class="styles.rsvpCount">{{ rsvpCounts.going }} going</span>
      <span :class="styles.rsvpCount">{{ rsvpCounts.maybe }} maybe</span>
      <span :class="styles.rsvpCount">{{ rsvpCounts.not_going }} can't go</span>
      <span v-if="totalRsvps === 0" :class="styles.rsvpEmpty">No responses yet</span>
    </div>
    <div v-if="$slots.actions" :class="styles.actions">
      <slot name="actions" />
    </div>
  </div>
</template>
