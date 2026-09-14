import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonChatMessage from '../components/AvalonChatMessage.vue'

const meta: Meta<typeof AvalonChatMessage> = {
  title: 'Avalon/ChatMessage',
  component: AvalonChatMessage,
  args: {
    authorId: '11111111-1111-1111-1111-111111111111',
    body: "Raid's at 8pm server time tonight, be there!",
    sentAtLabel: '8:04 PM',
  },
}
export default meta

type Story = StoryObj<typeof AvalonChatMessage>

export const Default: Story = {}
export const WithDisplayName: Story = { args: { authorDisplayName: 'Avalon User' } }
export const Deletable: Story = { args: { canDelete: true } }
export const LongMessage: Story = {
  args: {
    body: 'This is a much longer message meant to demonstrate how the chat bubble wraps text across multiple lines without breaking the layout of the surrounding channel view, even with a very long unbroken run of content.',
  },
}
