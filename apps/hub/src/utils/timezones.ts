// Issue #451: the full IANA timezone list for Profile.vue's timezone
// picker. `Intl.supportedValuesOf` (Baseline since 2023, all evergreen
// browsers) is the canonical source — no bundled data file to keep in
// sync. Falls back to a short common-zone list on an older engine that
// lacks it (Node/browser test environments included) rather than leaving
// the field with no options at all.
const FALLBACK_TIMEZONES = [
  'UTC',
  'America/New_York',
  'America/Chicago',
  'America/Denver',
  'America/Los_Angeles',
  'America/Sao_Paulo',
  'Europe/London',
  'Europe/Paris',
  'Europe/Berlin',
  'Europe/Moscow',
  'Africa/Cairo',
  'Africa/Johannesburg',
  'Asia/Dubai',
  'Asia/Kolkata',
  'Asia/Shanghai',
  'Asia/Tokyo',
  'Australia/Sydney',
  'Pacific/Auckland',
]

export function listIanaTimezones(): string[] {
  const supportedValuesOf = (
    Intl as unknown as { supportedValuesOf?: (key: string) => string[] }
  ).supportedValuesOf
  if (typeof supportedValuesOf === 'function') {
    try {
      return supportedValuesOf('timeZone')
    } catch {
      // Fall through to the static list below.
    }
  }
  return FALLBACK_TIMEZONES
}
