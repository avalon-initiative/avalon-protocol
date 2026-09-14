import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonMetricTile from '../components/AvalonMetricTile.vue'

const meta: Meta<typeof AvalonMetricTile> = {
  title: 'Avalon/MetricTile',
  component: AvalonMetricTile,
  args: {
    label: 'Players',
    value: 2481392,
    definition: 'Distinct identities with an active IntegratorBinding.',
    metricClass: 'durable-derived',
  },
}
export default meta

type Story = StoryObj<typeof AvalonMetricTile>

export const Default: Story = {}
export const Zero: Story = {
  args: {
    label: 'Achievements revoked',
    value: 0,
    definition: 'Count of achievement.revoked events issued by this issuer.',
  },
}
export const LongDefinition: Story = {
  args: {
    label: 'Unique achievement holders',
    value: 918204,
    definition: 'Distinct subjects with at least one valid attestation from this issuer, counted once regardless of how many achievements they hold.',
  },
}
