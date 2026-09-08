import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonGuildMemberRow from '../components/AvalonGuildMemberRow.vue'

const meta: Meta<typeof AvalonGuildMemberRow> = {
  title: 'Avalon/GuildMemberRow',
  component: AvalonGuildMemberRow,
  args: {
    identityId: '11111111-1111-1111-1111-111111111111',
    status: 'Online',
    roleName: 'member',
    roleVariant: 'member',
  },
}
export default meta

type Story = StoryObj<typeof AvalonGuildMemberRow>

export const Online: Story = { args: { status: 'Online' } }
export const Away: Story = { args: { status: 'Away' } }
export const Offline: Story = { args: { status: 'Offline' } }
export const OwnerRole: Story = { args: { roleName: 'owner', roleVariant: 'owner' } }
export const OfficerRole: Story = { args: { roleName: 'officer', roleVariant: 'officer' } }
export const MemberRole: Story = { args: { roleName: 'member', roleVariant: 'member' } }
export const WithDisplayName: Story = { args: { displayName: 'Avalon Player' } }
export const ManageableRow: Story = {
  args: { canChangeRole: true, canKick: true, roleName: 'officer', roleVariant: 'officer' },
}
export const KickOnlyRow: Story = { args: { canKick: true, roleName: 'member', roleVariant: 'member' } }
