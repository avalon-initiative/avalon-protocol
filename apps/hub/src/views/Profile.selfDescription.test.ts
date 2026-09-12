// Issue #277: the server has supported bio/favorite_genres/pronouns since
// #155, but the Hub never surfaced or edited them until now. Mirrors
// Profile.discoverable.test.ts's own pattern for a field the server
// echoes back rather than being assumed to have saved as sent.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { AvalonEditableField } from '@avalon/ui'
import Profile from './Profile.vue'
import { useSessionStore } from '../stores/session'
import { mockFetchByPath } from '../testing/fakes'

const baseProfile = {
  identity_id: 'id-1',
  identity_created_at: 'now',
  display_name: 'Nova',
  avatar_url: null,
  handle: 'Nova#4821',
  bio: 'Full-time dragon slayer.',
  pronouns: 'she/her',
  favorite_genres: ['rpg', 'strategy'],
  discoverable: false,
}

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/', component: Profile }],
  })
}

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
})

describe('Profile self-description fields (issue #277)', () => {
  it('renders bio, pronouns, and favorite genres from GET /me', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({ '/me': baseProfile, '/me/passkeys': [] })

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Profile, { global: { plugins: [router] } })

    await vi.waitFor(() => expect(wrapper.text()).toContain('Full-time dragon slayer.'))
    expect(wrapper.text()).toContain('she/her')
    // Genres render as checked checkboxes over the fixed vocabulary.
    const rpgCheckbox = wrapper.find('#genre-rpg')
    const actionCheckbox = wrapper.find('#genre-action')
    expect((rpgCheckbox.element as HTMLInputElement).checked).toBe(true)
    expect((actionCheckbox.element as HTMLInputElement).checked).toBe(false)
  })

  it('shows empty-state text when bio/pronouns are unset', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      '/me': { ...baseProfile, bio: null, pronouns: null, favorite_genres: [] },
      '/me/passkeys': [],
    })

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Profile, { global: { plugins: [router] } })

    await vi.waitFor(() => expect(wrapper.text()).toContain('No bio'))
    expect(wrapper.text()).toContain('No pronouns set')
  })

  /**
   * Like Profile.discoverable.test.ts, this needs `/me` to answer
   * differently for the initial GET versus the PATCH a save sends.
   */
  function mockMeSequence(bodies: unknown[]) {
    let call = 0
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((url: string) => {
        const path = new URL(url, 'http://test').pathname
        let body: unknown
        if (path === '/me') {
          body = bodies[Math.min(call, bodies.length - 1)]
          call += 1
        } else if (path === '/me/passkeys') {
          body = []
        } else {
          body = undefined
        }
        return Promise.resolve({
          ok: true,
          status: 200,
          json: () => Promise.resolve(body),
          text: () => Promise.resolve(body === undefined ? '' : JSON.stringify(body)),
        })
      }),
    )
  }

  it('saving the bio field reflects the server response', async () => {
    mockMeSequence([baseProfile, { ...baseProfile, bio: 'Updated bio.' }])
    useSessionStore().login('a-token')

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Profile, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Full-time dragon slayer.'))

    const bioField = wrapper
      .findAllComponents(AvalonEditableField)
      .find((f) => f.props('label') === 'Bio')
    expect(bioField).toBeTruthy()
    await bioField!.find('button').trigger('click') // "Edit"
    const input = bioField!.find('input[type="text"]')
    await input.setValue('Updated bio.')
    const saveButton = bioField!.findAll('button').find((b) => b.text() === 'Save')
    await saveButton!.trigger('click')

    await vi.waitFor(() => expect(wrapper.text()).toContain('Updated bio.'))
  })

  it('toggling and saving favorite genres reflects the server response', async () => {
    mockMeSequence([
      { ...baseProfile, favorite_genres: [] },
      { ...baseProfile, favorite_genres: ['horror'] },
    ])
    useSessionStore().login('a-token')

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Profile, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Favorite genres'))

    await wrapper.find('#genre-horror').trigger('change')
    const saveButton = wrapper.findAll('button').find((b) => b.text().includes('Save favorite genres'))
    await saveButton!.trigger('click')

    await vi.waitFor(() =>
      expect((wrapper.find('#genre-horror').element as HTMLInputElement).checked).toBe(true),
    )
  })
})
