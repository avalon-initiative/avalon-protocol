import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonColorPicker from '../components/AvalonColorPicker.vue'

const meta: Meta<typeof AvalonColorPicker> = {
  title: 'Avalon/ColorPicker',
  component: AvalonColorPicker,
  args: { label: 'Theme color', modelValue: '' },
}
export default meta

type Story = StoryObj<typeof AvalonColorPicker>

export const Default: Story = {}
export const WithValue: Story = { args: { modelValue: '#a1b2c3' } }
export const Error: Story = {
  args: { modelValue: 'not-a-color', error: 'Must be a 6-digit hex color, like #a1b2c3.' },
}
export const Disabled: Story = { args: { modelValue: '#a1b2c3', disabled: true } }
