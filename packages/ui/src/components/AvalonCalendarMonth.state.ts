// Pure derivation for AvalonCalendarMonth, kept out of the .vue file per
// this package's glue-only-script convention.
import { computed } from 'vue'
import { MONTH_NAMES, monthGrid } from '../utils/calendarGrid'
import type { AvalonCalendarMonthProps } from './AvalonCalendarMonth.types'

export function useCalendarMonth(props: AvalonCalendarMonthProps) {
  const grid = computed(() => monthGrid(props.year, props.month))
  const monthLabel = computed(() => `${MONTH_NAMES[props.month - 1]} ${props.year}`)
  const eventDateSet = computed(() => new Set(props.eventDates))

  function hasEvents(iso: string): boolean {
    return eventDateSet.value.has(iso)
  }

  function isSelected(iso: string): boolean {
    return props.selectedDate === iso
  }

  return { grid, monthLabel, hasEvents, isSelected }
}

export function prevMonth(year: number, month: number): { year: number; month: number } {
  return month === 1 ? { year: year - 1, month: 12 } : { year, month: month - 1 }
}

export function nextMonth(year: number, month: number): { year: number; month: number } {
  return month === 12 ? { year: year + 1, month: 1 } : { year, month: month + 1 }
}
