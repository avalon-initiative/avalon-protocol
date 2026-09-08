import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonTextField from '../components/AvalonTextField.vue'

const meta: Meta<typeof AvalonTextField> = {
  title: 'Avalon/TextField',
  component: AvalonTextField,
  args: { label: 'Display name', modelValue: '' },
}
export default meta

type Story = StoryObj<typeof AvalonTextField>

export const Default: Story = {}
export const WithValue: Story = { args: { modelValue: 'Avalon Player' } }
export const Error: Story = {
  args: { modelValue: '', error: 'Display name is required.' },
}
export const Disabled: Story = { args: { modelValue: 'Avalon Player', disabled: true } }
