// Component tests for packages/ui. They live here rather than in
// packages/ui because that package has no test runner configured (no
// vitest, no test script) — a pre-existing gap. This app already has a
// working vitest + jsdom setup and consumes these components the same way
// any real page does, via `@avalon/ui`.
import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import {
  AvalonAvatar,
  AvalonBottomNav,
  AvalonCard,
  AvalonFriendRequestRow,
  AvalonFriendRow,
  AvalonPresenceBadge,
  AvalonSidebarNav,
  AvalonUserChip,
} from '@avalon/ui'

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

describe('AvalonAvatar', () => {
  it('renders the initial letter when there is no image', () => {
    const wrapper = mount(AvalonAvatar, { props: { name: 'nova' } })
    expect(wrapper.find('img').exists()).toBe(false)
    expect(wrapper.text()).toBe('N')
  })

  it('renders the image when a src is given', () => {
    const wrapper = mount(AvalonAvatar, { props: { name: 'Nova', src: 'https://example.com/a.png' } })
    const img = wrapper.find('img')
    expect(img.exists()).toBe(true)
    expect(img.attributes('alt')).toBe('Nova')
  })

  it('falls back to "?" for an empty name', () => {
    const wrapper = mount(AvalonAvatar, { props: { name: '  ' } })
    expect(wrapper.text()).toBe('?')
  })
})

describe('AvalonCard', () => {
  it('renders the title, action slot, and body', () => {
    const wrapper = mount(AvalonCard, {
      props: { title: 'Friends Online' },
      slots: { default: '<p>body</p>', action: '<a>View all</a>' },
    })
    expect(wrapper.text()).toContain('Friends Online')
    expect(wrapper.text()).toContain('View all')
    expect(wrapper.text()).toContain('body')
  })

  it('renders no header at all without a title or action', () => {
    const wrapper = mount(AvalonCard, { slots: { default: 'just body' } })
    expect(wrapper.find('header').exists()).toBe(false)
  })
})

const navItems = [
  { label: 'Home', to: '/home', icon: 'home', active: true },
  { label: 'Games', to: '/games', icon: 'games', active: false, disabled: true },
  { label: 'Friends', to: '/friends', icon: 'friends', active: false },
] as const

describe('AvalonSidebarNav', () => {
  it('renders every label and a Soon tag on disabled items', () => {
    const wrapper = mount(AvalonSidebarNav, { props: { items: [...navItems] } })
    expect(wrapper.text()).toContain('Home')
    expect(wrapper.text()).toContain('Games')
    expect(wrapper.text()).toContain('Soon')
  })

  it('emits select with the clicked item\'s `to`', async () => {
    const wrapper = mount(AvalonSidebarNav, { props: { items: [...navItems] } })
    await wrapper.findAll('button')[2].trigger('click')
    expect(wrapper.emitted('select')).toEqual([['/friends']])
  })

  it('never emits select for a disabled item', async () => {
    const wrapper = mount(AvalonSidebarNav, { props: { items: [...navItems] } })
    const disabled = wrapper.findAll('button')[1]
    expect(disabled.attributes('disabled')).toBeDefined()
    await disabled.trigger('click')
    expect(wrapper.emitted('select')).toBeUndefined()
  })

  it('marks the active item as the current page', () => {
    const wrapper = mount(AvalonSidebarNav, { props: { items: [...navItems] } })
    expect(wrapper.findAll('button')[0].attributes('aria-current')).toBe('page')
  })
})

describe('AvalonBottomNav', () => {
  it('emits select for an enabled item and not for a disabled one', async () => {
    const wrapper = mount(AvalonBottomNav, { props: { items: [...navItems] } })
    const buttons = wrapper.findAll('button')
    await buttons[1].trigger('click')
    await buttons[2].trigger('click')
    expect(wrapper.emitted('select')).toEqual([['/friends']])
  })
})

describe('AvalonUserChip', () => {
  it('renders the name and detail line', () => {
    const wrapper = mount(AvalonUserChip, { props: { name: 'Nova', detail: 'Nova#4821' } })
    expect(wrapper.text()).toContain('Nova')
    expect(wrapper.text()).toContain('Nova#4821')
  })
})
