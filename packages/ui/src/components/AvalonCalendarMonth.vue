<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files —
// grid/derivation logic lives in AvalonCalendarMonth.state.ts and
// utils/calendarGrid.ts. A full navigable month view with a dot under any
// day that has an event (the "which days have something on them" glance a
// guild calendar tab needs) — distinct from AvalonDateTimeField's compact
// popover picker, though both build on the same calendarGrid utility.
import styles from '../styles/AvalonCalendarMonth.module.scss'
import type { AvalonCalendarMonthProps } from '../types/AvalonCalendarMonth.types'
import { useCalendarMonth, prevMonth, nextMonth } from '../state/AvalonCalendarMonth.state'
import { WEEKDAY_LABELS } from '../utils/calendarGrid'

const props = defineProps<AvalonCalendarMonthProps>()
const emit = defineEmits<{
  'update:year': [value: number]
  'update:month': [value: number]
  'select-date': [value: string]
}>()

const { grid, monthLabel, hasEvents, isSelected } = useCalendarMonth(props)

function goPrev() {
  const next = prevMonth(props.year, props.month)
  emit('update:year', next.year)
  emit('update:month', next.month)
}

function goNext() {
  const next = nextMonth(props.year, props.month)
  emit('update:year', next.year)
  emit('update:month', next.month)
}
</script>

<template>
  <div :class="styles.calendar">
    <div :class="styles.header">
      <button type="button" :class="styles.navButton" aria-label="Previous month" @click="goPrev">
        &lsaquo;
      </button>
      <span :class="styles.monthLabel">{{ monthLabel }}</span>
      <button type="button" :class="styles.navButton" aria-label="Next month" @click="goNext">
        &rsaquo;
      </button>
    </div>

    <div :class="styles.weekdayRow">
      <span v-for="label in WEEKDAY_LABELS" :key="label" :class="styles.weekday">{{ label }}</span>
    </div>

    <div v-for="(week, wi) in grid" :key="wi" :class="styles.weekRow">
      <button
        v-for="cell in week"
        :key="cell.iso"
        type="button"
        :class="[
          styles.dayCell,
          !cell.inMonth && styles.dayOutside,
          isSelected(cell.iso) && styles.daySelected,
        ]"
        @click="$emit('select-date', cell.iso)"
      >
        <span>{{ cell.day }}</span>
        <span v-if="hasEvents(cell.iso)" :class="styles.eventDot" />
      </button>
    </div>
  </div>
</template>
