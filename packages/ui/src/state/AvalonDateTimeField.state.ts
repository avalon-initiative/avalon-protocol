// Calendar/time-picker state for AvalonDateTimeField, kept out of the .vue
// file per this package's glue-only-script convention (same pattern
// AvalonEditableField.state.ts already established). Deliberately not a
// native <input type="datetime-local"> — browser-native date/time pickers
// vary wildly in usability across browsers, so this is a fully custom
// popover calendar (month/year as selects) + 12-hour HH:MM entry + AM/PM,
// while keeping the same "YYYY-MM-DDTHH:mm" (local time, 24h, no
// seconds/offset) value shape every caller already expects — the 12-hour
// display is purely a UI convenience layered on top in this module, never
// leaking into the emitted value.
import { computed, nextTick, ref, watch } from 'vue'
import type { Ref } from 'vue'
import { type DayCell, monthGrid, pad2, yearOptions } from '../utils/calendarGrid'

export type { DayCell } from '../utils/calendarGrid'
export { MONTH_NAMES, WEEKDAY_LABELS, monthGrid, daysInMonth } from '../utils/calendarGrid'

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

export function formatValue(parts: ParsedDateTime): string {
  return `${parts.year}-${pad2(parts.month)}-${pad2(parts.day)}T${pad2(parts.hour)}:${pad2(parts.minute)}`
}

export type Period = 'AM' | 'PM'

export function to12Hour(hour24: number): { hour12: number; period: Period } {
  const period: Period = hour24 >= 12 ? 'PM' : 'AM'
  const hour12 = hour24 % 12 === 0 ? 12 : hour24 % 12
  return { hour12, period }
}

export function to24Hour(hour12: number, period: Period): number {
  const clamped = ((hour12 - 1 + 12) % 12) + 1 // normalize into 1-12
  if (period === 'AM') return clamped === 12 ? 0 : clamped
  return clamped === 12 ? 12 : clamped + 12
}

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
  const years = computed(() => yearOptions(viewYear.value))
  // The one "YYYY-MM-DD" a day cell must match to render as selected.
  // Previously reconstructed per-cell in the template from
  // `parsed.year`/`parsed.month` + *that cell's own* day, which made every
  // in-month cell compare equal to itself whenever the viewed month
  // matched the parsed one — every day of the month lit up as selected.
  // Building it once, using `parsed.day`, avoids that class of bug.
  const selectedIso = computed(() => {
    const current = parsed.value
    if (!current) return null
    return `${current.year}-${pad2(current.month)}-${pad2(current.day)}`
  })

  const hour12Text = ref('12')
  const minuteText = ref('00')
  const period = ref<Period>('AM')

  // The typed HH/MM/AM-PM controls are local drafts, not directly bound to
  // the parsed value — otherwise every keystroke on a half-typed "1" (on
  // the way to "12") would round-trip through to24Hour/emit and fight the
  // input. They resync from the real value whenever it changes elsewhere
  // (a day click, or the popover opening) instead.
  function syncTimeDraftFromParsed() {
    const current = parsed.value
    const { hour12, period: p } = to12Hour(current?.hour ?? 0)
    hour12Text.value = String(hour12)
    minuteText.value = pad2(current?.minute ?? 0)
    period.value = p
  }
  watch(parsed, syncTimeDraftFromParsed, { immediate: true })

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

  function setViewYear(year: number) {
    viewYear.value = year
  }

  function setViewMonth(month: number) {
    viewMonth.value = month
  }

  function currentTimeParts() {
    const current = parsed.value
    return { hour: current?.hour ?? 0, minute: current?.minute ?? 0 }
  }

  function selectDay(cell: DayCell) {
    const { hour, minute } = currentTimeParts()
    const [y, m] = cell.iso.split('-').map(Number)
    emit('update:modelValue', formatValue({ year: y, month: m, day: cell.day, hour, minute }))
    if (!cell.inMonth) {
      viewYear.value = y
      viewMonth.value = m
    }
  }

  function commitTimeDraft() {
    const base = parsed.value ?? { year: viewYear.value, month: viewMonth.value, day: 1, hour: 0, minute: 0 }
    const hour12 = Math.min(12, Math.max(1, Number(hour12Text.value) || 12))
    const minute = Math.min(59, Math.max(0, Number(minuteText.value) || 0))
    const hour = to24Hour(hour12, period.value)
    emit('update:modelValue', formatValue({ ...base, hour, minute }))
  }

  function setHour12Text(value: string) {
    hour12Text.value = value
    commitTimeDraft()
  }

  function setMinuteText(value: string) {
    minuteText.value = value
    commitTimeDraft()
  }

  function setPeriod(next: Period) {
    period.value = next
    commitTimeDraft()
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
    years,
    grid,
    parsed,
    selectedIso,
    popover,
    trigger,
    hour12Text,
    minuteText,
    period,
    toggleOpen,
    close,
    onFocusOut,
    setViewYear,
    setViewMonth,
    selectDay,
    setHour12Text,
    setMinuteText,
    setPeriod,
    onTextInput,
  }
}
