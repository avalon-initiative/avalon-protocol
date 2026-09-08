import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonBottomNav from '../components/AvalonBottomNav.vue'

const items = [
  { label: 'Home', to: '/home', icon: 'home', active: true },
  { label: 'Friends', to: '/friends', icon: 'friends', active: false },
  { label: 'Guilds', to: '/guilds', icon: 'guilds', active: false, disabled: true },
  { label: 'Chat', to: '/chat', icon: 'chat', active: false, disabled: true },
  { label: 'Profile', to: '/profile', icon: 'profile', active: false },
] as const

const meta: Meta<typeof AvalonBottomNav> = {
  title: 'Avalon/BottomNav',
  component: AvalonBottomNav,
  args: { items: [...items] },
}
export default meta

type Story = StoryObj<typeof AvalonBottomNav>

export const Default: Story = {}
