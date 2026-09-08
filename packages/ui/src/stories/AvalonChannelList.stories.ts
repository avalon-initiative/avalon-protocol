import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonChannelList from '../components/AvalonChannelList.vue'

const channels = [
  { id: 'c1', name: 'general', archived: false },
  { id: 'c2', name: 'raid-planning', archived: false },
  { id: 'c3', name: 'old-season-1', archived: true },
]

const meta: Meta<typeof AvalonChannelList> = {
  title: 'Avalon/ChannelList',
  component: AvalonChannelList,
  args: { channels, activeChannelId: 'c1' },
}
export default meta

type Story = StoryObj<typeof AvalonChannelList>

export const Populated: Story = {}
export const Empty: Story = { args: { channels: [] } }
export const WithArchivedChannel: Story = { args: { channels } }
export const ManageableWithCreate: Story = { args: { canManage: true } }
