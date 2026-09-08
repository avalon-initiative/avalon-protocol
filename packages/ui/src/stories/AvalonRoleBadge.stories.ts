import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonRoleBadge from '../components/AvalonRoleBadge.vue'

const meta: Meta<typeof AvalonRoleBadge> = {
  title: 'Avalon/RoleBadge',
  component: AvalonRoleBadge,
  args: { name: 'member', variant: 'member' },
}
export default meta

type Story = StoryObj<typeof AvalonRoleBadge>

export const Owner: Story = { args: { name: 'owner', variant: 'owner' } }
export const Officer: Story = { args: { name: 'officer', variant: 'officer' } }
export const Member: Story = { args: { name: 'member', variant: 'member' } }
export const CustomRole: Story = { args: { name: 'raid leader', variant: 'officer' } }
