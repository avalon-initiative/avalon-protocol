import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonFriendRow from '../components/AvalonFriendRow.vue'

const meta: Meta<typeof AvalonFriendRow> = {
  title: 'Avalon/FriendRow',
  component: AvalonFriendRow,
  args: { identityId: '11111111-1111-1111-1111-111111111111', status: 'Online' },
}
export default meta

type Story = StoryObj<typeof AvalonFriendRow>

export const Online: Story = { args: { status: 'Online' } }
export const Away: Story = { args: { status: 'Away' } }
export const Offline: Story = { args: { status: 'Offline' } }
export const WithDisplayName: Story = { args: { status: 'Online', displayName: 'Avalon User' } }
