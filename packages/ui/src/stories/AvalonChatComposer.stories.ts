import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonChatComposer from '../components/AvalonChatComposer.vue'

const meta: Meta<typeof AvalonChatComposer> = {
  title: 'Avalon/ChatComposer',
  component: AvalonChatComposer,
  args: { modelValue: '', maxChars: 4000 },
}
export default meta

type Story = StoryObj<typeof AvalonChatComposer>

export const Empty: Story = {}
export const Typing: Story = { args: { modelValue: "Raid's at 8pm tonight!" } }
export const NearCap: Story = { args: { modelValue: 'a'.repeat(3980), maxChars: 4000 } }
export const OverCap: Story = { args: { modelValue: 'a'.repeat(4010), maxChars: 4000 } }
export const Sending: Story = { args: { modelValue: 'Sending this now', sending: true } }
export const WithError: Story = {
  args: { modelValue: 'oops', error: 'Something went wrong sending that message.' },
}
export const ArchivedChannel: Story = { args: { disabled: true, modelValue: '' } }
