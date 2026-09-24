// Per-member RSVP roster: who's going/maybe/can't-go for one
// guild event, opened by clicking an event card. Shared between the Events
// tab and the Calendar tab's selected-day list in Guild.vue — one composable
// backing one AvalonRsvpRosterPanel instance, rather than each tab building
// its own fetch-and-group logic.
//
// Fetches raw guild_event_rsvps rows (GET .../events/{eid}/rsvps, any
// current member) then resolves identity ids to display_name
// via the existing batched GET /identities/profiles — same
// pattern useGuildChat's resolveAuthorNames and api/guilds.ts's
// listMembersWithPresence already use.
import { ref, type Ref } from 'vue'
import { groupRsvpRoster } from '../api/guildEvents'
import type { AvalonRsvpRosterGroup } from '@avalon-initiative/common-ui'
import { useSessionStore } from '../api/session'

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
    const s = session.session
    if (!s) return
    loading.value = true
    try {
      const entries = await s.eventRsvps(guildId.value, eventId)
      const unresolvedIds = [...new Set(entries.map((e) => e.identityId))]
      const namesById: Record<string, string> = {}
      if (unresolvedIds.length > 0) {
        const profiles = await s.profiles(unresolvedIds)
        for (const profile of Array.isArray(profiles) ? profiles : []) {
          namesById[profile.identityId] = profile.displayName
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
