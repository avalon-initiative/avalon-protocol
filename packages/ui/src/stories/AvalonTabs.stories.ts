import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonTabs from '../components/AvalonTabs.vue'

const meta: Meta<typeof AvalonTabs> = {
  title: 'Avalon/Tabs',
  component: AvalonTabs,
  args: {
    tabs: [
      { label: 'Profile', to: '/profile', active: true },
      { label: 'Friends', to: '/friends', active: false },
    ],
  },
}
export default meta

type Story = StoryObj<typeof AvalonTabs>

export const Default: Story = {}
export const FriendsActive: Story = {
  args: {
    tabs: [
      { label: 'Profile', to: '/profile', active: false },
      { label: 'Friends', to: '/friends', active: true },
    ],
  },
}
