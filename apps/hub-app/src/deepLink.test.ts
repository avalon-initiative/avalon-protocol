import { describe, expect, it, vi } from 'vitest'
import { routeDeepLink } from './deepLink'

function fakeRouter() {
  return { push: vi.fn() } as unknown as import('vue-router').Router
}

describe('routeDeepLink', () => {
  it('routes a well-formed cross-node-login deep link', () => {
    const router = fakeRouter()
    routeDeepLink(router, 'avalon://cross-node-login?node=https%3A%2F%2Fnode-b.example&user_code=ABCD1234')

    expect(router.push).toHaveBeenCalledWith({
      name: 'cross-node-login',
      query: { node: 'https://node-b.example', user_code: 'ABCD1234' },
    })
  })

  it('ignores a deep link for a different, unrecognized host', () => {
    const router = fakeRouter()
    routeDeepLink(router, 'avalon://some-other-destination?foo=bar')
    expect(router.push).not.toHaveBeenCalled()
  })

  it('ignores a cross-node-login link missing node or user_code', () => {
    const router = fakeRouter()
    routeDeepLink(router, 'avalon://cross-node-login?node=https://node-b.example')
    expect(router.push).not.toHaveBeenCalled()

    routeDeepLink(router, 'avalon://cross-node-login?user_code=ABCD1234')
    expect(router.push).not.toHaveBeenCalled()
  })

  it('never throws on a malformed URL', () => {
    const router = fakeRouter()
    expect(() => routeDeepLink(router, 'not a url at all')).not.toThrow()
    expect(router.push).not.toHaveBeenCalled()
  })
})
