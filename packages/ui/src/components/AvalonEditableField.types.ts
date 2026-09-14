export interface AvalonEditableFieldProps {
  label: string
  /** The saved value. Read-only until the user presses Edit. */
  value: string
  /** Shown (muted) in read mode when `value` is empty. */
  emptyText?: string
  placeholder?: string
  /** True while the parent is persisting a save — disables Edit meanwhile. */
  saving?: boolean
  error?: string
}
