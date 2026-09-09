import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonEventCard from '../components/AvalonEventCard.vue'

const meta: Meta<typeof AvalonEventCard> = {
  title: 'Avalon/EventCard',
  component: AvalonEventCard,
  args: {
    title: 'Raid night',
    description: 'Weekly progression raid — bring consumables.',
    startsAt: '2026-09-15T20:00:00Z',
    endsAt: '2026-09-15T22:00:00Z',
    rsvpCounts: { going: 8, maybe: 2, not_going: 1 },
  },
}
export default meta

type Story = StoryObj<typeof AvalonEventCard>

export const Default: Story = {}
export const NoDescription: Story = { args: { description: undefined } }
export const NoEndTime: Story = { args: { endsAt: undefined } }
export const NoResponsesYet: Story = {
  args: { rsvpCounts: { going: 0, maybe: 0, not_going: 0 } },
}
