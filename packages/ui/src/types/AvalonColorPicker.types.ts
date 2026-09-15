export interface AvalonColorPickerProps {
  label: string
  // A 7-character hex color (`#rrggbb`), or empty for "unset".
  modelValue: string
  error?: string
  disabled?: boolean
}
