import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonWarningBanner from '../components/AvalonWarningBanner.vue'

const meta: Meta<typeof AvalonWarningBanner> = {
  title: 'Avalon/WarningBanner',
  component: AvalonWarningBanner,
  args: {
    title: 'You have only one passkey',
    message:
      "If you lose this device, you'll permanently lose this identity and everything durable it carries — friends, guild history, and achievements. Register a second passkey from another device.",
  },
}
export default meta

type Story = StoryObj<typeof AvalonWarningBanner>

export const Warning: Story = {}
export const Danger: Story = {
  args: {
    title: 'This is your only passkey',
    tone: 'danger',
  },
}
