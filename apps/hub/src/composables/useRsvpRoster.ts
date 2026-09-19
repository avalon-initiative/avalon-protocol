// Per-member RSVP roster (issue #248): who's going/maybe/can't-go for one
// guild event, opened by clicking an event card. Shared between the Events
// tab and the Calendar tab's selected-day list in Guild.vue — one composable
// backing one AvalonRsvpRosterPanel instance, rather than each tab building
// its own fetch-and-group logic.
//
// Fetches raw guild_event_rsvps rows (GET .../events/{eid}/rsvps, any
// current member) then resolves identity ids to display_name (issue #510)
// via the existing batched GET /identities/profiles (issue #161) — same
// pattern useGuildChat's resolveAuthorNames and api/guilds.ts's
// listMembersWithPresence already use.
import { ref, type Ref } from 'vue'
import * as api from '@avalon/api-client'
import { groupRsvpRoster } from '../api/guildEvents'
import type { AvalonRsvpRosterGroup } from '@avalon/ui'
import { useSessionStore } from '@avalon/api-client'

export function useRsvpRoster(guildId: Ref<string>) {
  const session = useSessionStore()

  const open = ref(false)
  const loading = ref(false)
  const error = ref('')
  const eventTitle = ref('')
  const groups = ref<AvalonRsvpRosterGroup[]>([])

  // Guards against a slow response for an event the reader already closed
  // or moved on from landing after the fact — same "isStaleFor" concern
  // useGuildChat documents, using a monotonic request id instead since
  // there's no natural "current id" ref to compare against here.
  let requestId = 0

  async function openFor(eventId: string, title: string) {
    const thisRequest = ++requestId
    open.value = true
    eventTitle.value = title
    error.value = ''
    groups.value = []
    if (!session.token) return
    loading.value = true
    try {
      const entries = await api.listEventRsvps(session.token, guildId.value, eventId)
      const unresolvedIds = [...new Set(entries.map((e) => e.identity_id))]
      const namesById: Record<string, string> = {}
      if (unresolvedIds.length > 0) {
        const profiles = await api.getProfiles(session.token, unresolvedIds)
        for (const profile of profiles) {
          namesById[profile.identity_id] = profile.display_name
        }
      }
      if (thisRequest !== requestId) return
      groups.value = groupRsvpRoster(entries, namesById)
    } catch (e) {
      if (thisRequest !== requestId) return
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      if (thisRequest === requestId) loading.value = false
    }
  }

  function close() {
    open.value = false
  }

  return { open, loading, error, eventTitle, groups, openFor, close }
}
