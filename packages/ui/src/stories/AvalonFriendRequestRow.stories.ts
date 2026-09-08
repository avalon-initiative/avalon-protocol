import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonFriendRequestRow from '../components/AvalonFriendRequestRow.vue'

const meta: Meta<typeof AvalonFriendRequestRow> = {
  title: 'Avalon/FriendRequestRow',
  component: AvalonFriendRequestRow,
  args: { identityId: '11111111-1111-1111-1111-111111111111', direction: 'incoming' },
}
export default meta

type Story = StoryObj<typeof AvalonFriendRequestRow>

export const Incoming: Story = { args: { direction: 'incoming' } }
export const Outgoing: Story = { args: { direction: 'outgoing' } }
