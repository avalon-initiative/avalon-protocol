import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonIcon from '../components/AvalonIcon.vue'

const meta: Meta<typeof AvalonIcon> = {
  title: 'Avalon/Icon',
  component: AvalonIcon,
  args: { name: 'home', size: 24 },
}
export default meta

type Story = StoryObj<typeof AvalonIcon>

export const Home: Story = {}
export const AllIcons: Story = {
  render: () => ({
    components: { AvalonIcon },
    template: `
      <div style="display:flex;gap:16px;flex-wrap:wrap">
        <AvalonIcon v-for="n in ['home','games','guilds','friends','chat','discover','profile','search','bell','plus','device','activity','logo']" :key="n" :name="n" :size="24" />
      </div>
    `,
  }),
}
