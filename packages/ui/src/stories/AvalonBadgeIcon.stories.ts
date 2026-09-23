import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonBadgeIcon from '../components/AvalonBadgeIcon.vue'

const meta: Meta<typeof AvalonBadgeIcon> = {
  title: 'Avalon/BadgeIcon',
  component: AvalonBadgeIcon,
  args: { tier: 'achievement', size: 48 },
}
export default meta

type Story = StoryObj<typeof AvalonBadgeIcon>

export const Achievement: Story = {}
export const Rare: Story = { args: { tier: 'rare' } }
export const Epic: Story = { args: { tier: 'epic' } }
export const Legendary: Story = { args: { tier: 'legendary' } }
export const Event: Story = { args: { tier: 'event' } }
export const Rank: Story = { args: { tier: 'rank' } }
export const Guild: Story = { args: { tier: 'guild' } }
export const Special: Story = { args: { tier: 'special' } }
export const Small: Story = { args: { tier: 'epic', size: 24 } }
