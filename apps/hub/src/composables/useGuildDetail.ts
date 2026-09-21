// Guild overview + roster + channels for Guild.vue (issue #24): loads once,
// then polls — matching the ticket's own design note ("poll for updates,
// no WebSocket needed for milestone 1, matching how presence/friends
// already poll"). Roster presence is merged client-side via
// apps/hub/src/api/guilds.ts::listMembersWithPresence, the same pattern
// apps/hub/src/api/friends.ts's listFriendsWithPresence already uses.
import { computed, onMounted, onUnmounted, ref, watch, type Ref } from 'vue'
import type { Guild, GuildChannel, GuildEvent, GuildJoinRequest, GameBreakdown, Role } from '@avalon/sdk'
import { listMembersWithPresence, permissionsForMember } from '../api/guilds'
import type { GuildMember } from '../api/guilds'
import { useSessionStore } from '../api/session'

const POLL_INTERVAL_MS = 5 * 60_000

export function useGuildDetail(guildId: Ref<string>) {
  const session = useSessionStore()

  const guild = ref<Guild | null>(null)
  const roles = ref<Role[]>([])
  const members = ref<GuildMember[]>([])
  const channels = ref<GuildChannel[]>([])
  const events = ref<GuildEvent[]>([])
  const selfId = ref('')
  const loading = ref(true)
  const error = ref('')

  // Issue #206: fetched separately from the rest of the guild, since a 403
  // here (not permitted, and the guild hasn't made it public) is an
  // expected, common outcome — not a page-level error like the others
  // above. `null` means "nothing to show" (either state), distinguished
  // from an actual empty breakdown (`breakdown: []`) by `integratorBreakdownError`.
  const integratorBreakdown = ref<GameBreakdown | null>(null)
  const integratorBreakdownError = ref('')

  // Issue #242: pending join requests, `manage_members`-gated server-side.
  // Same "fetched separately, a 403 just means nothing to show" posture as
  // integratorBreakdown above — most callers aren't managers, so this is an
  // expected, common outcome, not a page-level error.
  const joinRequests = ref<GuildJoinRequest[]>([])

  // Resolved display names for join-request applicants — `joinRequests`
  // only carries a raw identity id (`GuildJoinRequest.applicant`), same
  // "resolve via GET /identities/profiles" pattern useGuildChat.ts's
  // authorNames and useConversations.ts's participantNames already use.
  const applicantNames = ref<Record<string, string>>({})

  async function resolveApplicantNames(requests: GuildJoinRequest[]) {
    const s = session.session
    if (!s) return
    const unknown = [...new Set(requests.map((r) => r.applicant))].filter(
      (id) => !(id in applicantNames.value),
    )
    if (unknown.length === 0) return
    try {
      const profiles = await s.profiles(unknown)
      const resolved: Record<string, string> = {}
      for (const profile of Array.isArray(profiles) ? profiles : []) {
        resolved[profile.identityId] = profile.displayName
      }
      applicantNames.value = { ...applicantNames.value, ...resolved }
    } catch {
      // Best-effort — requests still render with the raw id.
    }
  }

  // Issue #256: the caller's own pending join request for this guild, if
  // any — self-scoped, not manage_members-gated, so (unlike joinRequests
  // above) this is fetched for every caller, not just managers.
  const myJoinRequest = ref<GuildJoinRequest | null>(null)

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
    const s = session.session
    if (!s) return
    try {
      members.value = await listMembersWithPresence(s, guildId.value)
    } catch {
      members.value = []
    }
  }

  async function refreshChannels() {
    const s = session.session
    if (!s) return
    try {
      const result = await s.listChannels(guildId.value)
      channels.value = Array.isArray(result) ? result : []
    } catch {
      channels.value = []
    }
  }

  async function refreshEvents() {
    const s = session.session
    if (!s) return
    try {
      const result = await s.listEvents(guildId.value)
      events.value = Array.isArray(result) ? result : []
    } catch {
      events.value = []
    }
  }

  async function refreshGameBreakdown() {
    const s = session.session
    if (!s) return
    try {
      integratorBreakdown.value = await s.getGameBreakdown(guildId.value)
      integratorBreakdownError.value = ''
    } catch (e) {
      integratorBreakdown.value = null
      integratorBreakdownError.value = e instanceof Error ? e.message : 'Something went wrong.'
    }
  }

  async function refreshJoinRequests() {
    const s = session.session
    if (!s) return
    try {
      const result = await s.listJoinRequests(guildId.value)
      joinRequests.value = Array.isArray(result) ? result : []
      await resolveApplicantNames(joinRequests.value)
    } catch {
      joinRequests.value = []
    }
  }

  async function refreshMyJoinRequest() {
    const s = session.session
    if (!s) return
    try {
      myJoinRequest.value = await s.myJoinRequest(guildId.value)
    } catch {
      myJoinRequest.value = null
    }
  }

  async function refresh() {
    const s = session.session
    if (!s) return
    try {
      const [guildResp, rolesResp] = await Promise.all([s.getGuild(guildId.value), s.listRoles(guildId.value)])
      guild.value = guildResp
      roles.value = Array.isArray(rolesResp) ? rolesResp : []
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
    const s = session.session
    if (!s) return
    loading.value = true
    error.value = ''
    try {
      // resumeAccountSession(WithSigningKey) already validated this
      // session's own identity — no separate GET /me round trip needed
      // just to learn the caller's own id, unlike the old api-client flow.
      if (!selfId.value) {
        selfId.value = s.identity().id
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
    applicantNames,
    myJoinRequest,
    refresh,
  }
}
