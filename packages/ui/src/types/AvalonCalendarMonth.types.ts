export interface AvalonCalendarMonthProps {
  year: number
  month: number // 1-12
  // "YYYY-MM-DD" dates that have at least one event — rendered as a dot
  // under the day number. The caller computes this (e.g. from a guild's
  // event list) since this component has no idea what "an event" is.
  eventDates: string[]
  // "YYYY-MM-DD" — the currently selected/focused day, if any.
  selectedDate?: string | null
}
