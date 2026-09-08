import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonForm from '../components/AvalonForm.vue'

const meta: Meta<typeof AvalonForm> = {
  title: 'Avalon/Form',
  component: AvalonForm,
  args: { submitLabel: 'Continue' },
  render: (args) => ({
    components: { AvalonForm },
    setup: () => ({ args }),
    template: '<AvalonForm v-bind="args"><p>Form contents go here.</p></AvalonForm>',
  }),
}
export default meta

type Story = StoryObj<typeof AvalonForm>

export const Default: Story = {}
export const Submitting: Story = { args: { submitting: true } }
export const Error: Story = { args: { error: 'Something went wrong. Please try again.' } }
