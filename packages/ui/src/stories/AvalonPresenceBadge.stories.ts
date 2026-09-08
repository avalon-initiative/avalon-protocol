import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonPresenceBadge from '../components/AvalonPresenceBadge.vue'

const meta: Meta<typeof AvalonPresenceBadge> = {
  title: 'Avalon/PresenceBadge',
  component: AvalonPresenceBadge,
  args: { status: 'Online' },
}
export default meta

type Story = StoryObj<typeof AvalonPresenceBadge>

export const Online: Story = { args: { status: 'Online' } }
export const Away: Story = { args: { status: 'Away' } }
export const Offline: Story = { args: { status: 'Offline' } }
