import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonSidebarNav from '../components/AvalonSidebarNav.vue'

const items = [
  { label: 'Home', to: '/home', icon: 'home', active: true },
  { label: 'Integrators', to: '/integrations', icon: 'integrators', active: false, disabled: true },
  { label: 'Guilds', to: '/guilds', icon: 'guilds', active: false, disabled: true },
  { label: 'Friends', to: '/friends', icon: 'friends', active: false },
  { label: 'Chat', to: '/chat', icon: 'chat', active: false, disabled: true },
  { label: 'Discover', to: '/discover', icon: 'discover', active: false, disabled: true },
  { label: 'Profile', to: '/profile', icon: 'profile', active: false },
] as const

const meta: Meta<typeof AvalonSidebarNav> = {
  title: 'Avalon/SidebarNav',
  component: AvalonSidebarNav,
  args: { items: [...items] },
}
export default meta

type Story = StoryObj<typeof AvalonSidebarNav>

export const Default: Story = {}
