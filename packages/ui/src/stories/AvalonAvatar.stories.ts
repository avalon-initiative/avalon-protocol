import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonAvatar from '../components/AvalonAvatar.vue'

const meta: Meta<typeof AvalonAvatar> = {
  title: 'Avalon/Avatar',
  component: AvalonAvatar,
  args: { name: 'Nova', size: 'md' },
}
export default meta

type Story = StoryObj<typeof AvalonAvatar>

export const InitialFallback: Story = {}
export const WithImage: Story = {
  args: { src: 'https://placehold.co/96x96/5b6cff/ffffff?text=N' },
}
export const ExtraLarge: Story = { args: { size: 'xl' } }
