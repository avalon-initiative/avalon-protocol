// Guild overview + roster + channels for Guild.vue (issue #24): loads once,
// then polls — matching the ticket's own design note ("poll for updates,
// no WebSocket needed for milestone 1, matching how presence/friends
// already poll"). Roster presence is merged client-side via
// apps/hub/src/api/guilds.ts::listMembersWithPresence, the same pattern
// apps/hub/src/api/friends.ts's listFriendsWithPresence already uses.
import { computed, onMounted, onUnmounted, ref, watch, type Ref } from 'vue'
import * as api from '../api/client'
import { listMembersWithPresence, permissionsForMember } from '../api/guilds'
import type { GuildMember } from '../api/guilds'
import type {
  ChannelResponse,
  EventResponse,
  GameBreakdownResponse,
  GuildJoinRequestResponse,
  GuildResponse,
  RoleResponse,
} from '../api/types'
import { useSessionStore } from '../stores/session'

const POLL_INTERVAL_MS = 5 * 60_000

export function useGuildDetail(guildId: Ref<string>) {
  const session = useSessionStore()

  const guild = ref<GuildResponse | null>(null)
  const roles = ref<RoleResponse[]>([])
  const members = ref<GuildMember[]>([])
  const channels = ref<ChannelResponse[]>([])
  const events = ref<EventResponse[]>([])
  const selfId = ref('')
  const loading = ref(true)
  const error = ref('')

  // Issue #206: fetched separately from the rest of the guild, since a 403
  // here (not permitted, and the guild hasn't made it public) is an
  // expected, common outcome — not a page-level error like the others
  // above. `null` means "nothing to show" (either state), distinguished
  // from an actual empty breakdown (`breakdown: []`) by `integratorBreakdownError`.
  const integratorBreakdown = ref<GameBreakdownResponse | null>(null)
  const integratorBreakdownError = ref('')

  // Issue #242: pending join requests, `manage_members`-gated server-side.
  // Same "fetched separately, a 403 just means nothing to show" posture as
  // integratorBreakdown above — most callers aren't managers, so this is an
  // expected, common outcome, not a page-level error.
  const joinRequests = ref<GuildJoinRequestResponse[]>([])

  // Issue #256: the caller's own pending join request for this guild, if
  // any — self-scoped, not manage_members-gated, so (unlike joinRequests
  // above) this is fetched for every caller, not just managers.
  const myJoinRequest = ref<GuildJoinRequestResponse | null>(null)

  let pollHandle: ReturnType<typeof setInterval> | undefined

  // Issue #391 (and this same bug's own regression against it): channels/
  // events/members can all 403 for a non-member server-side
  // (require_member, or #87's roster_visibility for members specifically)
  // — a 403 here for a non-member browsing a recruiting guild is expected,
  // not a page-level failure. `members` used to be bundled into the
  // Promise.all below alongside guild/roles, so an expected roster-403
  // rejected the whole thing and `guild`/`roles` never got set either,
  // breaking the entire page (including the Join button) for exactly the
  // "browsing a guild I'm not in yet" case this is supposed to support —
  // fetched separately here for the same reason channels/events already
  // are.
  async function refreshMembers() {
    if (!session.token) return
    try {
      members.value = await listMembersWithPresence(session.token, guildId.value)
    } catch {
      members.value = []
    }
  }

  async function refreshChannels() {
    if (!session.token) return
    try {
      channels.value = await api.listChannels(session.token, guildId.value)
    } catch {
      channels.value = []
    }
  }

  async function refreshEvents() {
    if (!session.token) return
    try {
      events.value = await api.listEvents(session.token, guildId.value)
    } catch {
      events.value = []
    }
  }

  async function refreshGameBreakdown() {
    if (!session.token) return
    try {
      integratorBreakdown.value = await api.getGameBreakdown(session.token, guildId.value)
      integratorBreakdownError.value = ''
    } catch (e) {
      integratorBreakdown.value = null
      integratorBreakdownError.value = e instanceof Error ? e.message : 'Something went wrong.'
    }
  }

  async function refreshJoinRequests() {
    if (!session.token) return
    try {
      joinRequests.value = await api.listJoinRequests(session.token, guildId.value)
    } catch {
      joinRequests.value = []
    }
  }

  async function refreshMyJoinRequest() {
    if (!session.token) return
    try {
      myJoinRequest.value = await api.getMyJoinRequest(session.token, guildId.value)
    } catch {
      myJoinRequest.value = null
    }
  }

  async function refresh() {
    if (!session.token) return
    try {
      const [guildResp, rolesResp] = await Promise.all([
        api.getGuild(session.token, guildId.value),
        api.listRoles(session.token, guildId.value),
      ])
      guild.value = guildResp
      roles.value = rolesResp
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    }
    // Independent of the Promise.all above: a permission rejection here
    // must never surface as the page-level `error` the template already
    // treats as fatal.
    await refreshMembers()
    await refreshChannels()
    await refreshEvents()
    await refreshGameBreakdown()
    await refreshJoinRequests()
    await refreshMyJoinRequest()
  }

  async function load() {
    if (!session.token) return
    loading.value = true
    error.value = ''
    try {
      if (!selfId.value) {
        const profile = await api.getMe(session.token)
        selfId.value = profile.identity_id
      }
      await refresh()
    } finally {
      loading.value = false
    }
  }

  // A user navigating from one guild page straight to another (same
  // route component, different :id param) doesn't remount — vue-router
  // reuses the instance — so the reload has to be driven by watching the
  // param rather than only onMounted.
  watch(guildId, load)

  onMounted(() => {
    load()
    pollHandle = setInterval(refresh, POLL_INTERVAL_MS)
  })

  onUnmounted(() => {
    if (pollHandle) clearInterval(pollHandle)
  })

  const selfPermissions = computed(() => permissionsForMember(selfId.value, members.value, roles.value))
  const isOwner = computed(() => guild.value !== null && selfId.value === guild.value.owner)

  return {
    guild,
    roles,
    members,
    channels,
    events,
    selfId,
    selfPermissions,
    isOwner,
    loading,
    error,
    integratorBreakdown,
    integratorBreakdownError,
    joinRequests,
    myJoinRequest,
    refresh,
  }
}
