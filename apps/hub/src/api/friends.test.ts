import { describe, expect, it } from 'vitest'
import { mergeFriend, splitFriendRequests } from './friends'
import type { FriendRequest, Friendship } from '@avalon-initiative/protocol-sdk'

const SELF = 'self-id'
const OTHER = 'other-id'

describe('mergeFriend', () => {
  it('resolves the other party regardless of a/b order', () => {
    const asA: Friendship = { a: SELF, b: OTHER, since: 't' }
    const asB: Friendship = { a: OTHER, b: SELF, since: 't' }

    expect(mergeFriend(asA, SELF, new Map()).identityId).toBe(OTHER)
    expect(mergeFriend(asB, SELF, new Map()).identityId).toBe(OTHER)
  })

  it('defaults to Offline when the presence map has no entry for the friend', () => {
    const friendship: Friendship = { a: SELF, b: OTHER, since: 't' }
    const friend = mergeFriend(friendship, SELF, new Map())
    expect(friend.status).toBe('Offline')
  })

  it('uses the presence map entry when present', () => {
    const friendship: Friendship = { a: SELF, b: OTHER, since: 't' }
    const friend = mergeFriend(friendship, SELF, new Map([[OTHER, 'Online']]))
    expect(friend.status).toBe('Online')
  })

  it('leaves display name undefined when the profile map has no entry (#161)', () => {
    const friendship: Friendship = { a: SELF, b: OTHER, since: 't' }
    const friend = mergeFriend(friendship, SELF, new Map())
    expect(friend.displayName).toBeUndefined()
  })

  it('resolves display name from the profile map when present (#161)', () => {
    const friendship: Friendship = { a: SELF, b: OTHER, since: 't' }
    const friend = mergeFriend(friendship, SELF, new Map(), new Map([[OTHER, 'best-friend-42']]))
    expect(friend.displayName).toBe('best-friend-42')
  })
})

describe('splitFriendRequests', () => {
  it('labels a request the caller sent as outgoing', () => {
    const requests: FriendRequest[] = [{ id: 'r1', from: SELF, to: OTHER, requestedAt: 't' }]
    const [view] = splitFriendRequests(requests, SELF)
    expect(view.direction).toBe('outgoing')
    expect(view.otherIdentityId).toBe(OTHER)
  })

  it('labels a request the caller received as incoming', () => {
    const requests: FriendRequest[] = [{ id: 'r1', from: OTHER, to: SELF, requestedAt: 't' }]
    const [view] = splitFriendRequests(requests, SELF)
    expect(view.direction).toBe('incoming')
    expect(view.otherIdentityId).toBe(OTHER)
  })

  it('attaches the resolved display name for the other party', () => {
    const requests: FriendRequest[] = [{ id: 'r1', from: OTHER, to: SELF, requestedAt: 't' }]
    const [view] = splitFriendRequests(requests, SELF, new Map([[OTHER, 'Bea']]))
    expect(view.displayName).toBe('Bea')
  })

  it('leaves displayName undefined when no profile was resolved', () => {
    const requests: FriendRequest[] = [{ id: 'r1', from: SELF, to: OTHER, requestedAt: 't' }]
    expect(splitFriendRequests(requests, SELF)[0].displayName).toBeUndefined()
  })
})
