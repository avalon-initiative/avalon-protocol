import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonCapabilityConsentRow from '../components/AvalonCapabilityConsentRow.vue'

const meta: Meta<typeof AvalonCapabilityConsentRow> = {
  title: 'Avalon/CapabilityConsentRow',
  component: AvalonCapabilityConsentRow,
  args: {
    capability: 'friends.read',
    description: 'See your friends list',
    checked: false,
  },
}
export default meta

type Story = StoryObj<typeof AvalonCapabilityConsentRow>

export const Unchecked: Story = {}

export const Checked: Story = { args: { checked: true } }
