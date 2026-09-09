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
  AvalonCapabilityConsentRow,
  AvalonCard,
  AvalonChannelList,
  AvalonChatComposer,
  AvalonChatMessage,
  AvalonConnectionCard,
  AvalonFilterBar,
  AvalonForm,
  AvalonFriendRequestRow,
  AvalonFriendRow,
  AvalonGuildCard,
  AvalonGuildMemberRow,
  AvalonPresenceBadge,
  AvalonRoleBadge,
  AvalonSidebarNav,
  AvalonSuggestionRow,
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

describe('AvalonSuggestionRow', () => {
  it('falls back to the identity id when no display name is given', () => {
    const wrapper = mount(AvalonSuggestionRow, { props: { identityId: 'id-1' } })
    expect(wrapper.text()).toContain('id-1')
  })

  it('prefers the display name when one is given', () => {
    const wrapper = mount(AvalonSuggestionRow, {
      props: { identityId: 'id-1', displayName: 'Avalon Player' },
    })
    expect(wrapper.text()).toContain('Avalon Player')
  })

  it('emits add when the add button is clicked', async () => {
    const wrapper = mount(AvalonSuggestionRow, { props: { identityId: 'id-1' } })
    await wrapper.find('button').trigger('click')
    expect(wrapper.emitted('add')).toHaveLength(1)
  })

  it('shows a disabled "Requested" state once requested, instead of Add', () => {
    const wrapper = mount(AvalonSuggestionRow, {
      props: { identityId: 'id-1', requested: true },
    })
    expect(wrapper.text()).toContain('Requested')
    expect(wrapper.text()).not.toContain('Add')
    expect(wrapper.find('button').attributes('disabled')).toBeDefined()
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

describe('AvalonRoleBadge', () => {
  it.each(['owner', 'officer', 'member'] as const)('renders the %s variant', (variant) => {
    const wrapper = mount(AvalonRoleBadge, { props: { name: variant, variant } })
    expect(wrapper.text()).toBe(variant)
  })
})

describe('AvalonGuildCard', () => {
  it('renders name, tag, and member count', () => {
    const wrapper = mount(AvalonGuildCard, {
      props: { name: 'Ashen Vanguard', tag: 'ASHV', memberCount: 3 },
    })
    expect(wrapper.text()).toContain('Ashen Vanguard')
    expect(wrapper.text()).toContain('ASHV')
    expect(wrapper.text()).toContain('3 members')
  })

  it('uses singular "member" for a count of 1', () => {
    const wrapper = mount(AvalonGuildCard, { props: { name: 'Solo', tag: 'SOLO', memberCount: 1 } })
    expect(wrapper.text()).toContain('1 member')
    expect(wrapper.text()).not.toContain('1 members')
  })

  it('emits select when clicked', async () => {
    const wrapper = mount(AvalonGuildCard, { props: { name: 'A', tag: 'AA', memberCount: 1 } })
    await wrapper.trigger('click')
    expect(wrapper.emitted('select')).toHaveLength(1)
  })

  it('shows a Recruiting badge only when recruiting is true', () => {
    const recruiting = mount(AvalonGuildCard, {
      props: { name: 'A', tag: 'AA', memberCount: 1, recruiting: true },
    })
    expect(recruiting.text()).toContain('Recruiting')

    const notRecruiting = mount(AvalonGuildCard, {
      props: { name: 'A', tag: 'AA', memberCount: 1, recruiting: false },
    })
    expect(notRecruiting.text()).not.toContain('Recruiting')

    const omitted = mount(AvalonGuildCard, { props: { name: 'A', tag: 'AA', memberCount: 1 } })
    expect(omitted.text()).not.toContain('Recruiting')
  })
})

describe('AvalonGuildMemberRow', () => {
  const baseProps = { identityId: 'id-1', status: 'Online' as const, roleName: 'member' }

  it('renders the role name and falls back to identity id for the display name', () => {
    const wrapper = mount(AvalonGuildMemberRow, { props: baseProps })
    expect(wrapper.text()).toContain('member')
    expect(wrapper.text()).toContain('id-1')
  })

  it('shows no management buttons by default', () => {
    const wrapper = mount(AvalonGuildMemberRow, { props: baseProps })
    expect(wrapper.findAll('button')).toHaveLength(0)
  })

  it('shows only Change role when canChangeRole is set without canKick', () => {
    const wrapper = mount(AvalonGuildMemberRow, { props: { ...baseProps, canChangeRole: true } })
    expect(wrapper.text()).toContain('Change role')
    expect(wrapper.text()).not.toContain('Kick')
  })

  it('shows only Kick when canKick is set without canChangeRole', () => {
    const wrapper = mount(AvalonGuildMemberRow, { props: { ...baseProps, canKick: true } })
    expect(wrapper.text()).not.toContain('Change role')
    expect(wrapper.text()).toContain('Kick')
  })

  it('emits kick when the kick button is clicked', async () => {
    const wrapper = mount(AvalonGuildMemberRow, { props: { ...baseProps, canKick: true } })
    await wrapper.find('button').trigger('click')
    expect(wrapper.emitted('kick')).toHaveLength(1)
  })

  it('uses the display name when set, instead of the identity id', () => {
    const wrapper = mount(AvalonGuildMemberRow, { props: { ...baseProps, displayName: 'Alice' } })
    expect(wrapper.text()).toContain('Alice')
    expect(wrapper.text()).not.toContain('id-1')
  })

  it('shortens a long identity id when no display name is set', () => {
    const longId = 'identity:0123456789abcdef'
    const wrapper = mount(AvalonGuildMemberRow, { props: { ...baseProps, identityId: longId } })
    expect(wrapper.text()).not.toContain(longId)
    expect(wrapper.text()).toContain(longId.slice(0, 8))
  })
})

describe('AvalonChannelList', () => {
  const channels = [
    { id: 'c1', name: 'general', archived: false },
    { id: 'c2', name: 'old', archived: true },
  ]

  it('renders an empty message when there are no channels', () => {
    const wrapper = mount(AvalonChannelList, { props: { channels: [] } })
    expect(wrapper.text()).toContain('No channels yet.')
  })

  it('renders every channel name and marks archived ones', () => {
    const wrapper = mount(AvalonChannelList, { props: { channels } })
    expect(wrapper.text()).toContain('general')
    expect(wrapper.text()).toContain('old')
    expect(wrapper.text()).toContain('Archived')
  })

  it('emits select with the clicked channel id', async () => {
    const wrapper = mount(AvalonChannelList, { props: { channels } })
    await wrapper.findAll('button')[0].trigger('click')
    expect(wrapper.emitted('select')).toEqual([['c1']])
  })

  it('only shows the create-channel affordance when canManage is true', () => {
    const withoutManage = mount(AvalonChannelList, { props: { channels } })
    expect(withoutManage.text()).not.toContain('New channel')

    const withManage = mount(AvalonChannelList, { props: { channels, canManage: true } })
    expect(withManage.text()).toContain('New channel')
  })
})

describe('AvalonChatMessage', () => {
  it('renders the author id and body when no display name is given', () => {
    const wrapper = mount(AvalonChatMessage, {
      props: { authorId: 'id-1', body: 'hello guild', sentAtLabel: '8:00 PM' },
    })
    expect(wrapper.text()).toContain('id-1')
    expect(wrapper.text()).toContain('hello guild')
    expect(wrapper.text()).toContain('8:00 PM')
  })

  it('shows no delete button by default', () => {
    const wrapper = mount(AvalonChatMessage, {
      props: { authorId: 'id-1', body: 'hi', sentAtLabel: 't' },
    })
    expect(wrapper.find('button').exists()).toBe(false)
  })

  it('emits delete when canDelete is set and the button is clicked', async () => {
    const wrapper = mount(AvalonChatMessage, {
      props: { authorId: 'id-1', body: 'hi', sentAtLabel: 't', canDelete: true },
    })
    await wrapper.find('button').trigger('click')
    expect(wrapper.emitted('delete')).toHaveLength(1)
  })
})

describe('AvalonChatComposer', () => {
  it('emits update:modelValue as the textarea is typed into', async () => {
    const wrapper = mount(AvalonChatComposer, { props: { modelValue: '', maxChars: 4000 } })
    await wrapper.find('textarea').setValue('hello')
    expect(wrapper.emitted('update:modelValue')).toEqual([['hello']])
  })

  it('disables Send for an empty or whitespace-only body', () => {
    const wrapper = mount(AvalonChatComposer, { props: { modelValue: '   ', maxChars: 4000 } })
    expect(wrapper.find('button[type="submit"]').attributes('disabled')).toBeDefined()
  })

  it('disables Send once the body exceeds maxChars', () => {
    const wrapper = mount(AvalonChatComposer, {
      props: { modelValue: 'a'.repeat(11), maxChars: 10 },
    })
    expect(wrapper.find('button[type="submit"]').attributes('disabled')).toBeDefined()
  })

  it('emits send when a valid body is submitted', async () => {
    const wrapper = mount(AvalonChatComposer, { props: { modelValue: 'hello', maxChars: 4000 } })
    await wrapper.find('form').trigger('submit')
    expect(wrapper.emitted('send')).toHaveLength(1)
  })

  it('shows the character counter against maxChars', () => {
    const wrapper = mount(AvalonChatComposer, { props: { modelValue: 'hello', maxChars: 4000 } })
    expect(wrapper.text()).toContain('5 / 4000')
  })
})

describe('AvalonForm', () => {
  it('renders both the submit button and secondary-actions slot content in the same row', () => {
    const wrapper = mount(AvalonForm, {
      props: { submitLabel: 'Save' },
      slots: { 'secondary-actions': '<button type="button">Cancel</button>' },
    })
    const submit = wrapper.find('button[type="submit"]')
    const cancel = wrapper.find('button[type="button"]')
    expect(submit.exists()).toBe(true)
    expect(cancel.exists()).toBe(true)
    // Both buttons are children of the same row container.
    expect(submit.element.parentElement).toBe(cancel.element.parentElement)
  })

  it('renders normally with no secondary-actions slot provided', () => {
    const wrapper = mount(AvalonForm, { props: { submitLabel: 'Save' } })
    expect(wrapper.find('button[type="submit"]').exists()).toBe(true)
    expect(wrapper.findAll('button')).toHaveLength(1)
  })
})

describe('AvalonFilterBar', () => {
  it('emits update:query as the search input changes', async () => {
    const wrapper = mount(AvalonFilterBar, { props: { query: '' } })
    await wrapper.find('input').setValue('alice')
    expect(wrapper.emitted('update:query')?.[0]).toEqual(['alice'])
  })

  it('renders no sort control when sortOptions is omitted', () => {
    const wrapper = mount(AvalonFilterBar, { props: { query: '' } })
    expect(wrapper.find('select').exists()).toBe(false)
  })

  it('renders a sort control and emits update:sortValue when sortOptions is set', async () => {
    const wrapper = mount(AvalonFilterBar, {
      props: {
        query: '',
        sortOptions: [
          { value: 'role', label: 'By role' },
          { value: 'name', label: 'By identity id' },
        ],
        sortValue: 'role',
      },
    })
    const select = wrapper.find('select')
    expect(select.exists()).toBe(true)
    await select.setValue('name')
    expect(wrapper.emitted('update:sortValue')?.[0]).toEqual(['name'])
  })
})

describe('AvalonCapabilityConsentRow', () => {
  const baseProps = { capability: 'friends.read', description: 'See your friends list', checked: false }

  it('renders the capability and its description', () => {
    const wrapper = mount(AvalonCapabilityConsentRow, { props: baseProps })
    expect(wrapper.text()).toContain('friends.read')
    expect(wrapper.text()).toContain('See your friends list')
  })

  it('is unchecked by default', () => {
    const wrapper = mount(AvalonCapabilityConsentRow, { props: baseProps })
    expect((wrapper.find('input[type=checkbox]').element as HTMLInputElement).checked).toBe(false)
  })

  it('reflects a checked prop', () => {
    const wrapper = mount(AvalonCapabilityConsentRow, { props: { ...baseProps, checked: true } })
    expect((wrapper.find('input[type=checkbox]').element as HTMLInputElement).checked).toBe(true)
  })

  it('emits update:checked when toggled', async () => {
    const wrapper = mount(AvalonCapabilityConsentRow, { props: baseProps })
    await wrapper.find('input[type=checkbox]').setValue(true)
    expect(wrapper.emitted('update:checked')?.[0]).toEqual([true])
  })
})

describe('AvalonConnectionCard', () => {
  const baseProps = {
    gameName: 'Ashen Realms',
    slug: 'ashen-realms',
    establishedAt: '2026-09-01',
    grants: [{ capability: 'friends.read', description: 'See your friends list' }],
  }

  it('renders the game name, slug, and grant descriptions', () => {
    const wrapper = mount(AvalonConnectionCard, { props: baseProps })
    expect(wrapper.text()).toContain('Ashen Realms')
    expect(wrapper.text()).toContain('ashen-realms')
    expect(wrapper.text()).toContain('See your friends list')
  })

  it('shows a message when there are no active grants', () => {
    const wrapper = mount(AvalonConnectionCard, { props: { ...baseProps, grants: [] } })
    expect(wrapper.text()).toContain('No active capability grants.')
  })

  it('emits revoke-grant with the capability when a revoke button is clicked', async () => {
    const wrapper = mount(AvalonConnectionCard, { props: baseProps })
    await wrapper.find('button').trigger('click')
    expect(wrapper.emitted('revoke-grant')?.[0]).toEqual(['friends.read'])
  })

  it('emits disconnect when the disconnect button is clicked', async () => {
    const wrapper = mount(AvalonConnectionCard, { props: baseProps })
    const buttons = wrapper.findAll('button')
    await buttons[buttons.length - 1].trigger('click')
    expect(wrapper.emitted('disconnect')).toHaveLength(1)
  })
})
