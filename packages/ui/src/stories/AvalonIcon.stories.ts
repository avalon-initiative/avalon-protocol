import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonIcon from '../components/AvalonIcon.vue'

const meta: Meta<typeof AvalonIcon> = {
  title: 'Avalon/Icon',
  component: AvalonIcon,
  args: { name: 'home', size: 24 },
}
export default meta

type Story = StoryObj<typeof AvalonIcon>

export const Home: Story = {}
export const AllIcons: Story = {
  render: () => ({
    components: { AvalonIcon },
    template: `
      <div style="display:flex;gap:16px;flex-wrap:wrap">
        <AvalonIcon v-for="n in allIconNames" :key="n" :name="n" :size="24" />
      </div>
    `,
    setup() {
      const allIconNames = [
        'home', 'integrators', 'guilds', 'friends', 'chat', 'discover', 'profile', 'search',
        'bell', 'plus', 'device', 'activity', 'logo', 'alert', 'pencil', 'check', 'close',
        'settings', 'voice', 'video', 'messages', 'calendar', 'achievements', 'library',
        'wallet', 'more', 'community', 'faction', 'event', 'reward', 'leaderboards', 'map',
        'join', 'leave', 'invite', 'share', 'bookmark', 'follow', 'muted', 'block',
      ]
      return { allIconNames }
    },
  }),
}
