// Calendar/time-picker state for AvalonDateTimeField, kept out of the .vue
// file per this package's glue-only-script convention (same pattern
// AvalonEditableField.state.ts already established). Deliberately not a
// native <input type="datetime-local"> — browser-native date/time pickers
// vary wildly in usability across browsers, so this is a fully custom
// popover calendar + hour/minute selects instead, while keeping the same
// "YYYY-MM-DDTHH:mm" (local time, no seconds/offset) value shape every
// caller already expects.
import { computed, nextTick, ref } from 'vue'
import type { Ref } from 'vue'

export interface ParsedDateTime {
  year: number
  month: number // 1-12
  day: number
  hour: number // 0-23
  minute: number
}

const VALUE_PATTERN = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/

export function parseValue(value: string): ParsedDateTime | null {
  const match = VALUE_PATTERN.exec(value)
  if (!match) return null
  const [, y, mo, d, h, mi] = match
  const year = Number(y)
  const month = Number(mo)
  const day = Number(d)
  const hour = Number(h)
  const minute = Number(mi)
  if (month < 1 || month > 12 || day < 1 || day > 31 || hour > 23 || minute > 59) return null
  return { year, month, day, hour, minute }
}

function pad2(n: number): string {
  return String(n).padStart(2, '0')
}

export function formatValue(parts: ParsedDateTime): string {
  return `${parts.year}-${pad2(parts.month)}-${pad2(parts.day)}T${pad2(parts.hour)}:${pad2(parts.minute)}`
}

export function daysInMonth(year: number, month: number): number {
  return new Date(year, month, 0).getDate()
}

export interface DayCell {
  day: number
  inMonth: boolean
  iso: string // "YYYY-MM-DD", for :key and equality checks against the selected day
}

// Sunday-first 6-row grid, including the trailing days of the previous
// month and leading days of the next so every row is a full week — the
// standard calendar-widget layout.
export function monthGrid(year: number, month: number): DayCell[][] {
  const firstOfMonth = new Date(year, month - 1, 1)
  const startOffset = firstOfMonth.getDay() // 0 = Sunday
  const totalDays = daysInMonth(year, month)
  const prevMonthDays = daysInMonth(month === 1 ? year - 1 : year, month === 1 ? 12 : month - 1)

  const cells: DayCell[] = []
  for (let i = startOffset - 1; i >= 0; i--) {
    const day = prevMonthDays - i
    const [y, m] = month === 1 ? [year - 1, 12] : [year, month - 1]
    cells.push({ day, inMonth: false, iso: `${y}-${pad2(m)}-${pad2(day)}` })
  }
  for (let day = 1; day <= totalDays; day++) {
    cells.push({ day, inMonth: true, iso: `${year}-${pad2(month)}-${pad2(day)}` })
  }
  let nextDay = 1
  while (cells.length % 7 !== 0 || cells.length < 42) {
    const [y, m] = month === 12 ? [year + 1, 1] : [year, month + 1]
    cells.push({ day: nextDay, inMonth: false, iso: `${y}-${pad2(m)}-${pad2(nextDay)}` })
    nextDay++
    if (cells.length >= 42) break
  }

  const weeks: DayCell[][] = []
  for (let i = 0; i < cells.length; i += 7) {
    weeks.push(cells.slice(i, i + 7))
  }
  return weeks
}

export const MONTH_NAMES = [
  'January',
  'February',
  'March',
  'April',
  'May',
  'June',
  'July',
  'August',
  'September',
  'October',
  'November',
  'December',
]

export const WEEKDAY_LABELS = ['Su', 'Mo', 'Tu', 'We', 'Th', 'Fr', 'Sa']

export interface DateTimeFieldEmit {
  (event: 'update:modelValue', value: string): void
}

export function useDateTimeField(currentValue: () => string, emit: DateTimeFieldEmit) {
  const now = new Date()
  const initial = parseValue(currentValue())

  const open = ref(false)
  const viewYear = ref(initial?.year ?? now.getFullYear())
  const viewMonth = ref(initial?.month ?? now.getMonth() + 1)
  const popover: Ref<HTMLElement | null> = ref(null)
  const trigger: Ref<HTMLElement | null> = ref(null)

  const parsed = computed(() => parseValue(currentValue()))
  const grid = computed(() => monthGrid(viewYear.value, viewMonth.value))
  const monthLabel = computed(() => `${MONTH_NAMES[viewMonth.value - 1]} ${viewYear.value}`)

  function syncViewToCurrent() {
    const current = parseValue(currentValue())
    if (current) {
      viewYear.value = current.year
      viewMonth.value = current.month
    }
  }

  async function toggleOpen() {
    if (!open.value) syncViewToCurrent()
    open.value = !open.value
    if (open.value) {
      await nextTick()
      popover.value?.focus()
    }
  }

  function close() {
    open.value = false
  }

  function onFocusOut(event: FocusEvent) {
    const next = event.relatedTarget as Node | null
    if (next && (popover.value?.contains(next) || trigger.value?.contains(next))) return
    close()
  }

  function prevMonth() {
    if (viewMonth.value === 1) {
      viewMonth.value = 12
      viewYear.value -= 1
    } else {
      viewMonth.value -= 1
    }
  }

  function nextMonth() {
    if (viewMonth.value === 12) {
      viewMonth.value = 1
      viewYear.value += 1
    } else {
      viewMonth.value += 1
    }
  }

  function selectDay(cell: DayCell) {
    const base = parsed.value ?? { hour: 0, minute: 0, year: viewYear.value, month: viewMonth.value, day: cell.day }
    const [y, m] = cell.iso.split('-').map(Number)
    emit('update:modelValue', formatValue({ year: y, month: m, day: cell.day, hour: base.hour, minute: base.minute }))
    if (!cell.inMonth) {
      viewYear.value = y
      viewMonth.value = m
    }
  }

  function setHour(hour: number) {
    const base = parsed.value ?? { year: viewYear.value, month: viewMonth.value, day: 1, hour: 0, minute: 0 }
    emit('update:modelValue', formatValue({ ...base, hour }))
  }

  function setMinute(minute: number) {
    const base = parsed.value ?? { year: viewYear.value, month: viewMonth.value, day: 1, hour: 0, minute: 0 }
    emit('update:modelValue', formatValue({ ...base, minute }))
  }

  function onTextInput(value: string) {
    // Free typed entry (issue request: "free entry, shared component") —
    // anything parseable updates the model exactly like picking from the
    // calendar would; anything not yet a full valid value is passed
    // through as-is so the caller's own validation (not this component's)
    // decides what to do with a partial/invalid string on submit.
    emit('update:modelValue', value)
  }

  return {
    open,
    viewYear,
    viewMonth,
    grid,
    monthLabel,
    parsed,
    popover,
    trigger,
    toggleOpen,
    close,
    onFocusOut,
    prevMonth,
    nextMonth,
    selectDay,
    setHour,
    setMinute,
    onTextInput,
  }
}
