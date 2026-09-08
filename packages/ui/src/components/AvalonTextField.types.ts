export interface AvalonTextFieldProps {
  label: string
  modelValue: string
  type?: 'text' | 'password'
  placeholder?: string
  error?: string
  disabled?: boolean
}
