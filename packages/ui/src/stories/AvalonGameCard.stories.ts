import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonGameCard from '../components/AvalonGameCard.vue'

const meta: Meta<typeof AvalonGameCard> = {
  title: 'Avalon/GameCard',
  component: AvalonGameCard,
  args: {
    name: 'Ashen Realms',
    slug: 'ashen-realms',
    developer: 'Ashen Studios',
    status: 'active',
    registeredAt: 'Jan 12, 2026',
  },
}
export default meta

type Story = StoryObj<typeof AvalonGameCard>

export const Default: Story = {}
export const LongName: Story = {
  args: { name: 'The Ever-Expanding Realms of Ashen Vale and Beyond' },
}
export const Suspended: Story = { args: { status: 'suspended' } }
export const Revoked: Story = { args: { status: 'revoked' } }
