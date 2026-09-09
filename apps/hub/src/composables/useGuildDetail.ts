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
import type { ChannelResponse, GameBreakdownResponse, GuildResponse, RoleResponse } from '../api/types'
import { useSessionStore } from '../stores/session'

const POLL_INTERVAL_MS = 5 * 60_000

export function useGuildDetail(guildId: Ref<string>) {
  const session = useSessionStore()

  const guild = ref<GuildResponse | null>(null)
  const roles = ref<RoleResponse[]>([])
  const members = ref<GuildMember[]>([])
  const channels = ref<ChannelResponse[]>([])
  const selfId = ref('')
  const loading = ref(true)
  const error = ref('')

  // Issue #206: fetched separately from the rest of the guild, since a 403
  // here (not permitted, and the guild hasn't made it public) is an
  // expected, common outcome — not a page-level error like the others
  // above. `null` means "nothing to show" (either state), distinguished
  // from an actual empty breakdown (`breakdown: []`) by `gameBreakdownError`.
  const gameBreakdown = ref<GameBreakdownResponse | null>(null)
  const gameBreakdownError = ref('')

  let pollHandle: ReturnType<typeof setInterval> | undefined

  async function refreshGameBreakdown() {
    if (!session.token) return
    try {
      gameBreakdown.value = await api.getGameBreakdown(session.token, guildId.value)
      gameBreakdownError.value = ''
    } catch (e) {
      gameBreakdown.value = null
      gameBreakdownError.value = e instanceof Error ? e.message : 'Something went wrong.'
    }
  }

  async function refresh() {
    if (!session.token) return
    try {
      const [guildResp, rolesResp, membersResp, channelsResp] = await Promise.all([
        api.getGuild(session.token, guildId.value),
        api.listRoles(session.token, guildId.value),
        listMembersWithPresence(session.token, guildId.value),
        api.listChannels(session.token, guildId.value),
      ])
      guild.value = guildResp
      roles.value = rolesResp
      members.value = membersResp
      channels.value = channelsResp
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    }
    // Independent of the Promise.all above: a permission rejection here
    // must never surface as the page-level `error` the template already
    // treats as fatal.
    await refreshGameBreakdown()
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

  // A player navigating from one guild page straight to another (same
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
    selfId,
    selfPermissions,
    isOwner,
    loading,
    error,
    gameBreakdown,
    gameBreakdownError,
    refresh,
  }
}
