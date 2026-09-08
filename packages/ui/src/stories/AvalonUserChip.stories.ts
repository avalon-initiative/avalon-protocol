import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonUserChip from '../components/AvalonUserChip.vue'

const meta: Meta<typeof AvalonUserChip> = {
  title: 'Avalon/UserChip',
  component: AvalonUserChip,
  args: { name: 'Nova', detail: 'Nova#4821' },
}
export default meta

type Story = StoryObj<typeof AvalonUserChip>

export const Default: Story = {}
export const WithAvatar: Story = {
  args: { avatarSrc: 'https://placehold.co/96x96/5b6cff/ffffff?text=N' },
}
