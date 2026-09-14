import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonIntegratorCard from '../components/AvalonIntegratorCard.vue'

const meta: Meta<typeof AvalonIntegratorCard> = {
  title: 'Avalon/IntegratorCard',
  component: AvalonIntegratorCard,
  args: {
    name: 'Ashen Realms',
    slug: 'ashen-realms',
    developer: 'Ashen Studios',
    status: 'active',
    registeredAt: 'Jan 12, 2026',
  },
}
export default meta

type Story = StoryObj<typeof AvalonIntegratorCard>

export const Default: Story = {}
export const LongName: Story = {
  args: { name: 'The Ever-Expanding Realms of Ashen Vale and Beyond' },
}
export const Suspended: Story = { args: { status: 'suspended' } }
export const Revoked: Story = { args: { status: 'revoked' } }
