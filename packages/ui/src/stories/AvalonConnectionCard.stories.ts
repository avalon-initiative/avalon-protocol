import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonConnectionCard from '../components/AvalonConnectionCard.vue'

const meta: Meta<typeof AvalonConnectionCard> = {
  title: 'Avalon/ConnectionCard',
  component: AvalonConnectionCard,
  args: {
    gameName: 'Ashen Realms',
    slug: 'ashen-realms',
    establishedAt: '2026-09-01',
    grants: [
      { capability: 'friends.read', description: 'See your friends list' },
      { capability: 'presence.read', description: "See your friends' online status" },
    ],
  },
}
export default meta

type Story = StoryObj<typeof AvalonConnectionCard>

export const Default: Story = {}

export const NoActiveGrants: Story = { args: { grants: [] } }
