import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonRsvpRosterPanel from '../components/AvalonRsvpRosterPanel.vue'

const meta: Meta<typeof AvalonRsvpRosterPanel> = {
  title: 'Avalon/RsvpRosterPanel',
  component: AvalonRsvpRosterPanel,
  args: {
    open: true,
    eventTitle: 'Raid night',
    loading: false,
    error: '',
    groups: [
      { status: 'going', label: 'Going', names: ['Rowan#1234', 'Sable#0007'] },
      { status: 'maybe', label: 'Maybe', names: ['Quill#4821'] },
      { status: 'not_going', label: "Can't go", names: [] },
    ],
  },
}
export default meta

type Story = StoryObj<typeof AvalonRsvpRosterPanel>

export const Default: Story = {}

export const Loading: Story = {
  args: { loading: true, groups: [] },
}

export const Empty: Story = {
  args: {
    groups: [
      { status: 'going', label: 'Going', names: [] },
      { status: 'maybe', label: 'Maybe', names: [] },
      { status: 'not_going', label: "Can't go", names: [] },
    ],
  },
}
