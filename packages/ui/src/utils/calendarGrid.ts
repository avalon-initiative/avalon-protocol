// Pure month-grid calendar math, shared by AvalonDateTimeField's popover
// picker and AvalonCalendarMonth's full month view — kept here rather than
// duplicated in each component's .state.ts.
export function pad2(n: number): string {
  return String(n).padStart(2, '0')
}

export function daysInMonth(year: number, month: number): number {
  return new Date(year, month, 0).getDate()
}

export interface DayCell {
  day: number
  inMonth: boolean
  iso: string // "YYYY-MM-DD", for :key and equality checks
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

// Years offered in a year select: a reasonable window around "now" for
// scheduling guild events — not meant to cover arbitrary historical dates.
export function yearOptions(centerYear: number): number[] {
  const start = centerYear - 2
  return Array.from({ length: 9 }, (_, i) => start + i)
}
