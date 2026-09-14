import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonGuildCard from '../components/AvalonGuildCard.vue'

const meta: Meta<typeof AvalonGuildCard> = {
  title: 'Avalon/GuildCard',
  component: AvalonGuildCard,
  args: {
    name: 'Ashen Vanguard',
    tag: 'ASHV',
    description: 'A cross-integrator community for Ashen Realms and beyond.',
    memberCount: 42,
  },
}
export default meta

type Story = StoryObj<typeof AvalonGuildCard>

export const Default: Story = {}
export const NoDescription: Story = { args: { description: undefined } }
export const SingleMember: Story = { args: { memberCount: 1, description: undefined } }
export const LongName: Story = {
  args: {
    name: 'The Order of the Ever-Vigilant Watchers of the Realm',
    tag: 'OEVW',
  },
}
export const Recruiting: Story = { args: { recruiting: true } }
export const WithIcon: Story = {
  args: { iconUrl: 'https://placehold.co/48x48' },
}
export const WithBanner: Story = {
  args: { bannerUrl: 'https://placehold.co/400x120', iconUrl: 'https://placehold.co/48x48' },
}
