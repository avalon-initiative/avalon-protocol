import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonButton from '../components/AvalonButton.vue'

const meta: Meta<typeof AvalonButton> = {
  title: 'Avalon/Button',
  component: AvalonButton,
  args: { label: 'Connect with Avalon Protocol' },
}
export default meta

type Story = StoryObj<typeof AvalonButton>

export const Primary: Story = { args: { variant: 'primary' } }
export const Secondary: Story = { args: { variant: 'secondary' } }
export const Danger: Story = { args: { variant: 'danger', label: 'Revoke' } }
