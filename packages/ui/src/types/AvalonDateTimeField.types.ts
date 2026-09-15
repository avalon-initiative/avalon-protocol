export interface AvalonDateTimeFieldProps {
  label: string
  // "YYYY-MM-DDTHH:mm", local time, no seconds or timezone offset — same
  // shape `new Date(value)` already expects from callers like Guild.vue's
  // event form. Empty string means unset. A partial/invalid string is
  // passed straight through from free typed entry; validating it is the
  // caller's job (same posture every other Avalon* field takes), not this
  // component's.
  modelValue: string
  error?: string
  disabled?: boolean
}
