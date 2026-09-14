import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonSuggestionRow from '../components/AvalonSuggestionRow.vue'

const meta: Meta<typeof AvalonSuggestionRow> = {
  title: 'Avalon/SuggestionRow',
  component: AvalonSuggestionRow,
  args: { identityId: '11111111-1111-1111-1111-111111111111' },
}
export default meta

type Story = StoryObj<typeof AvalonSuggestionRow>

export const Default: Story = {}
export const WithDisplayName: Story = { args: { displayName: 'Avalon User' } }
export const Requested: Story = { args: { displayName: 'Avalon User', requested: true } }
