import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonModal from '../components/AvalonModal.vue'

const meta: Meta<typeof AvalonModal> = {
  title: 'Avalon/Modal',
  component: AvalonModal,
  args: { title: 'Edit guild', open: true },
}
export default meta

type Story = StoryObj<typeof AvalonModal>

export const Default: Story = {
  render: (args) => ({
    components: { AvalonModal },
    setup: () => ({ args }),
    template: `<AvalonModal v-bind="args"><p>Modal body content goes here.</p></AvalonModal>`,
  }),
}
