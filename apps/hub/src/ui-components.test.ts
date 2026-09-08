// Component tests for the packages/ui components added in issue #18
// (AvalonPresenceBadge, AvalonFriendRow, AvalonFriendRequestRow). They live
// here rather than in packages/ui because that package has no test runner
// configured at all yet (no vitest, no test script) — a pre-existing gap,
// same category as its missing Storybook config (see issue #55's PR). This
// app already has a working vitest + jsdom setup and consumes these
// components the same way any real page does, via `@avalon/ui`, so testing
// them from here is the pragmatic choice rather than standing up a second
// test pipeline for three components.
import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import { AvalonFriendRequestRow, AvalonFriendRow, AvalonPresenceBadge } from '@avalon/ui'

describe('AvalonPresenceBadge', () => {
  it.each(['Online', 'Away', 'Offline'] as const)('renders the %s status', (status) => {
    const wrapper = mount(AvalonPresenceBadge, { props: { status } })
    expect(wrapper.text()).toContain(status)
  })
})

describe('AvalonFriendRow', () => {
  it('renders as Offline when status is Offline', () => {
    const wrapper = mount(AvalonFriendRow, {
      props: { identityId: 'id-1', status: 'Offline' },
    })
    expect(wrapper.text()).toContain('Offline')
  })

  it('falls back to the identity id when no display name is given', () => {
    const wrapper = mount(AvalonFriendRow, {
      props: { identityId: 'id-1', status: 'Online' },
    })
    expect(wrapper.text()).toContain('id-1')
  })

  it('prefers the display name when one is given', () => {
    const wrapper = mount(AvalonFriendRow, {
      props: { identityId: 'id-1', status: 'Online', displayName: 'Avalon Player' },
    })
    expect(wrapper.text()).toContain('Avalon Player')
  })

  it('emits remove when the remove button is clicked', async () => {
    const wrapper = mount(AvalonFriendRow, {
      props: { identityId: 'id-1', status: 'Online' },
    })
    await wrapper.find('button').trigger('click')
    expect(wrapper.emitted('remove')).toHaveLength(1)
  })
})

describe('AvalonFriendRequestRow', () => {
  it('shows accept and decline for an incoming request', () => {
    const wrapper = mount(AvalonFriendRequestRow, {
      props: { identityId: 'id-1', direction: 'incoming' },
    })
    expect(wrapper.text()).toContain('Accept')
    expect(wrapper.text()).toContain('Decline')
  })

  it('shows only withdraw for an outgoing request', () => {
    const wrapper = mount(AvalonFriendRequestRow, {
      props: { identityId: 'id-1', direction: 'outgoing' },
    })
    expect(wrapper.text()).not.toContain('Accept')
    expect(wrapper.text()).toContain('Withdraw')
  })

  it('emits accept when the accept button is clicked', async () => {
    const wrapper = mount(AvalonFriendRequestRow, {
      props: { identityId: 'id-1', direction: 'incoming' },
    })
    await wrapper.find('button').trigger('click')
    expect(wrapper.emitted('accept')).toHaveLength(1)
  })

  it('emits remove when the withdraw button is clicked on an outgoing request', async () => {
    const wrapper = mount(AvalonFriendRequestRow, {
      props: { identityId: 'id-1', direction: 'outgoing' },
    })
    await wrapper.find('button').trigger('click')
    expect(wrapper.emitted('remove')).toHaveLength(1)
  })
})
