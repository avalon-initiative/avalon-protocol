<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// A guild calendar entry — title/time/RSVP counts. The caller
// supplies the RSVP control via the `actions` slot, keeping this component
// itself dumb about mutation (same "props in, click out" split
// AvalonGuildCard/AvalonChannelList already use).
import { computed } from 'vue'
import styles from '../styles/AvalonEventCard.module.scss'
import type { AvalonEventCardProps } from '../types/AvalonEventCard.types'

// `withDefaults` matters here specifically because `detailsVisible` is an
// optional `boolean` — Vue casts an *omitted* boolean prop to `false` at
// runtime regardless of the TS type, so every existing caller (which never
// passes this prop) would otherwise silently render as "details hidden"
// without an explicit `true` default.
const props = withDefaults(defineProps<AvalonEventCardProps>(), {
  detailsVisible: true,
})

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
    <p v-if="detailsVisible === false" :class="styles.detailsHidden">
      Details hidden — you don't have permission to view this event's content.
    </p>
    <template v-else>
      <p v-if="description" :class="styles.description">{{ description }}</p>
      <div :class="styles.rsvpSummary">
        <span :class="styles.rsvpCount">{{ rsvpCounts.going }} going</span>
        <span :class="styles.rsvpCount">{{ rsvpCounts.maybe }} maybe</span>
        <span :class="styles.rsvpCount">{{ rsvpCounts.not_going }} can't go</span>
        <span v-if="totalRsvps === 0" :class="styles.rsvpEmpty">No responses yet</span>
      </div>
    </template>
    <div v-if="$slots.actions" :class="styles.actions">
      <slot name="actions" />
    </div>
  </div>
</template>
