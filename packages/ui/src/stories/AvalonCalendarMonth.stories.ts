import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonCalendarMonth from '../components/AvalonCalendarMonth.vue'

const meta: Meta<typeof AvalonCalendarMonth> = {
  title: 'Avalon/CalendarMonth',
  component: AvalonCalendarMonth,
  args: {
    year: 2026,
    month: 9,
    eventDates: ['2026-09-05', '2026-09-15', '2026-09-15', '2026-09-28'],
    selectedDate: '2026-09-15',
  },
}
export default meta

type Story = StoryObj<typeof AvalonCalendarMonth>

export const Default: Story = {}
export const NoEvents: Story = { args: { eventDates: [], selectedDate: null } }
