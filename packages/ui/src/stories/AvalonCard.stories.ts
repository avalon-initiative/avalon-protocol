import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonCard from '../components/AvalonCard.vue'

const meta: Meta<typeof AvalonCard> = {
  title: 'Avalon/Card',
  component: AvalonCard,
  args: { title: 'Friends Online (3)' },
  render: (args) => ({
    components: { AvalonCard },
    setup: () => ({ args }),
    template: `
      <AvalonCard v-bind="args">
        <template #action><a href="#">View all</a></template>
        <p>Card body content goes here.</p>
      </AvalonCard>
    `,
  }),
}
export default meta

type Story = StoryObj<typeof AvalonCard>

export const Default: Story = {}
export const WithSubtitle: Story = {
  args: { title: 'Your devices', subtitle: 'Every device that can sign for this identity.' },
}
