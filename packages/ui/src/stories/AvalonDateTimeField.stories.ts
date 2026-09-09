import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonDateTimeField from '../components/AvalonDateTimeField.vue'

const meta: Meta<typeof AvalonDateTimeField> = {
  title: 'Avalon/DateTimeField',
  component: AvalonDateTimeField,
  args: { label: 'Starts at', modelValue: '' },
}
export default meta

type Story = StoryObj<typeof AvalonDateTimeField>

export const Default: Story = {}
export const WithValue: Story = { args: { modelValue: '2026-09-15T20:00' } }
export const Error: Story = {
  args: { modelValue: '', error: 'Pick a start date and time.' },
}
export const Disabled: Story = { args: { modelValue: '2026-09-15T20:00', disabled: true } }
