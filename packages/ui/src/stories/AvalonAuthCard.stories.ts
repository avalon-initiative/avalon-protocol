import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonAuthCard from '../components/AvalonAuthCard.vue'

const meta: Meta<typeof AvalonAuthCard> = {
  title: 'Avalon/AuthCard',
  component: AvalonAuthCard,
  args: { title: 'Create your Avalon identity' },
  render: (args) => ({
    components: { AvalonAuthCard },
    setup: () => ({ args }),
    template: '<AvalonAuthCard v-bind="args"><p>Card contents go here.</p></AvalonAuthCard>',
  }),
}
export default meta

type Story = StoryObj<typeof AvalonAuthCard>

export const Default: Story = {}
export const WithSubtitle: Story = {
  args: { subtitle: 'One identity, every integrator connected to Avalon.' },
}
