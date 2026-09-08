import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonFilterBar from '../components/AvalonFilterBar.vue'

const meta: Meta<typeof AvalonFilterBar> = {
  title: 'Avalon/FilterBar',
  component: AvalonFilterBar,
  args: { query: '', label: 'Search', placeholder: 'Search…' },
}
export default meta

type Story = StoryObj<typeof AvalonFilterBar>

export const Default: Story = {}

export const WithQuery: Story = { args: { query: 'alice' } }

export const WithSort: Story = {
  args: {
    sortOptions: [
      { value: 'role', label: 'By role' },
      { value: 'name', label: 'By identity id' },
    ],
    sortValue: 'role',
  },
}
