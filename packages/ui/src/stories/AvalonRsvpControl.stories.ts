import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonRsvpControl from '../components/AvalonRsvpControl.vue'

const meta: Meta<typeof AvalonRsvpControl> = {
  title: 'Avalon/RsvpControl',
  component: AvalonRsvpControl,
  args: {},
}
export default meta

type Story = StoryObj<typeof AvalonRsvpControl>

export const NoResponseYet: Story = {}
export const Going: Story = { args: { currentStatus: 'going' } }
export const Maybe: Story = { args: { currentStatus: 'maybe' } }
export const NotGoing: Story = { args: { currentStatus: 'not_going' } }
export const Disabled: Story = { args: { currentStatus: 'going', disabled: true } }
