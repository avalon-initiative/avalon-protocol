import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonEditableField from '../components/AvalonEditableField.vue'

const meta: Meta<typeof AvalonEditableField> = {
  title: 'Avalon/EditableField',
  component: AvalonEditableField,
  args: { label: 'Display name', value: 'Nova', placeholder: 'How other users see you' },
}
export default meta

type Story = StoryObj<typeof AvalonEditableField>

export const Default: Story = {}
export const Empty: Story = { args: { value: '', emptyText: 'Unlabeled device' } }
export const Saving: Story = { args: { saving: true } }
export const WithError: Story = {
  args: { value: 'javascript:alert(1)', error: 'avatar_url must be an http(s) URL' },
}
