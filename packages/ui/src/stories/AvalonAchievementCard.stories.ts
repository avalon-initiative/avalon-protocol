import type { Meta, StoryObj } from '@storybook/vue3'
import AvalonAchievementCard from '../components/AvalonAchievementCard.vue'

const meta: Meta<typeof AvalonAchievementCard> = {
  title: 'Avalon/AchievementCard',
  component: AvalonAchievementCard,
  args: {
    achievementName: 'Dragon Slayer',
    issuerName: 'Ashen Realms',
    issuerSlug: 'ashen-realms',
    issuedAt: 'Mar 14, 2027',
    status: 'valid',
    history: [{ event: 'issued', at: 'Mar 14, 2027' }],
  },
}
export default meta

type Story = StoryObj<typeof AvalonAchievementCard>

export const Valid: Story = {}

export const Revoked: Story = {
  args: {
    status: 'invalid',
    invalidReason: 'attestation has been revoked',
    history: [
      { event: 'issued', at: 'Mar 14, 2027' },
      { event: 'revoked', at: 'May 2, 2027', reasonCode: 'cheating_detected', reason: 'Player used unauthorized tooling' },
    ],
  },
}

export const InvalidIssuer: Story = {
  args: { status: 'invalid', invalidReason: 'issuer has been revoked' },
}
