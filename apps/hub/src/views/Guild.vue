<script setup lang="ts">
// Guild overview / roster / roles / channels (issue #24), restructured into
// tabs by issue #241 — Overview / Members / Channels / Events / Roles /
// Settings, same activeTab-ref + local.tabs/tab/tabActive pattern
// Guilds.vue's "My guilds"/"Discover" tabs already use. Management controls
// are shown only when the caller's own role grants the matching permission
// (crates/server/src/guilds.rs's fixed permission set) — the server
// re-checks independently and is the real authority, so a 403 here is
// possible and shown as a plain error rather than crashing the page.
import { computed, nextTick, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import {
  AvalonButton,
  AvalonCalendarMonth,
  AvalonCard,
  AvalonChannelList,
  AvalonChatComposer,
  AvalonChatMessage,
  AvalonDateTimeField,
  AvalonEditableField,
  AvalonEventCard,
  AvalonFilterBar,
  AvalonForm,
  AvalonGuildMemberRow,
  AvalonIcon,
  AvalonModal,
  AvalonRsvpControl,
  AvalonRsvpRosterPanel,
  AvalonTextField,
} from '@avalon/ui'
import * as api from '../api/client'
import type { RoleResponse } from '../api/types'
import ChannelPermissionOverrides from '../components/ChannelPermissionOverrides.vue'
import { MESSAGE_BODY_MAX_CHARS } from '../api/guildChat'
import { localDateKey, sortByStartsAt, validateEventForm } from '../api/guildEvents'
import {
  addFavoriteGameId,
  canChangeMemberRole,
  canKickMember,
  canPinMoreFavorites,
  filterMembersByIdentityId,
  formatFavoriteGameEntry,
  formatGameBreakdownEntry,
  formatPlayingSummary,
  groupMembersByRole,
  groupMembersPlayingByGame,
  hasGuildPermission,
  hasNoGameBreakdownData,
  membershipStatusText,
  pinnableBreakdownEntries,
  removeFavoriteGameId,
  reorderFavoriteGameIds,
  roleVariantForIndex,
  sortMembers,
  sortMembersByPresence,
  type MemberSortOrder,
} from '../api/guilds'
import { useGuildChat } from '../composables/useGuildChat'
import { useGuildDetail } from '../composables/useGuildDetail'
import { useRsvpRoster } from '../composables/useRsvpRoster'
import { useSessionStore } from '../stores/session'
import local from './Guild.module.scss'
import styles from './page.module.scss'

// Issue #250 added `event_manage` (split out of `manage_channels`) and
// `channel_post` (the announcement-only-channels proof point) to the base
// GuildPermission vocabulary — both editable here as ordinary base
// permissions, same as the original four. Per-resource overrides on top
// of these are a separate surface (ChannelPermissionOverrides.vue on the
// Channels tab), not this guild-wide matrix.
const PERMISSION_OPTIONS = [
  'manage_guild',
  'manage_roles',
  'manage_members',
  'manage_channels',
  'event_manage',
  'channel_post',
]

const route = useRoute()
const router = useRouter()
const session = useSessionStore()

const guildId = computed(() => route.params.id as string)
const {
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
  gameBreakdown,
  gameBreakdownError,
  joinRequests,
  myJoinRequest,
  refresh,
} = useGuildDetail(guildId)

const actionError = ref('')

const canManageGuild = computed(
  () => guild.value !== null && hasGuildPermission(guild.value, selfId.value, selfPermissions.value, 'manage_guild'),
)
const canManageRoles = computed(
  () => guild.value !== null && hasGuildPermission(guild.value, selfId.value, selfPermissions.value, 'manage_roles'),
)
const canManageMembers = computed(
  () =>
    guild.value !== null && hasGuildPermission(guild.value, selfId.value, selfPermissions.value, 'manage_members'),
)
const canManageChannels = computed(
  () =>
    guild.value !== null && hasGuildPermission(guild.value, selfId.value, selfPermissions.value, 'manage_channels'),
)
const isMember = computed(() => members.value.some((m) => m.identityId === selfId.value))

// --- Tabs (issue #241) ---------------------------------------------------
// Overview/Members/Events/Roles/Settings are plain client-side state, same
// as Guilds.vue's "My guilds"/"Discover" tabs — no route involved. Channels
// is the one exception: a channel is independently deep-linkable
// (`/guilds/:id/channels/:cid`), so selecting one is reflected into the URL
// via router.replace (no push — switching channels shouldn't pile up
// browser history entries) rather than kept purely in memory.
type TabKey = 'overview' | 'members' | 'channels' | 'events' | 'calendar' | 'roles' | 'settings'
const TABS: { key: TabKey; label: string }[] = [
  { key: 'overview', label: 'Overview' },
  { key: 'members', label: 'Members' },
  { key: 'channels', label: 'Channels' },
  { key: 'events', label: 'Events' },
  { key: 'calendar', label: 'Calendar' },
  { key: 'roles', label: 'Roles' },
  { key: 'settings', label: 'Settings' },
]

const activeTab = ref<TabKey>(route.name === 'guild-channel' ? 'channels' : 'overview')

function selectTab(tab: TabKey) {
  activeTab.value = tab
  // Leaving the Channels tab drops the channel-specific URL back to the
  // plain guild route — the channel selection itself is kept in memory
  // (selectedChannelId below) so returning to Channels doesn't lose it.
  if (tab !== 'channels' && route.name === 'guild-channel') {
    router.replace({ name: 'guild', params: { id: guildId.value } })
  }
}

// --- Channels (issue #22/#24, folded into this tab by #241) --------------
// AvalonChannelList renders as a persistent left sidebar; the active
// channel's messages/composer render beside it via useGuildChat, which is
// reused as-is (no polling/pagination logic duplicated here) — it already
// reloads whenever channelId changes, which is exactly what happens when
// the reader clicks a different channel in the sidebar. No component
// remount, no route round-trip per channel switch.
const selectedChannelId = ref((route.params.cid as string | undefined) ?? '')

// A direct/deep link (or browser back/forward) into `/guilds/:id/channels/:cid`
// should open the Channels tab with that channel pre-selected.
watch(
  () => route.params.cid as string | undefined,
  (cid) => {
    if (cid) {
      activeTab.value = 'channels'
      selectedChannelId.value = cid
    }
  },
)

// Once the channel list loads, default to the first (preferring a
// non-archived one) if nothing is selected yet — e.g. arriving at
// `/guilds/:id` with no `:cid` at all.
watch(
  channels,
  (list) => {
    if (selectedChannelId.value || list.length === 0) return
    selectedChannelId.value = list.find((c) => !c.archived)?.id ?? list[0].id
  },
  { immediate: true },
)

function selectChannel(channelId: string) {
  selectedChannelId.value = channelId
  router.replace({ name: 'guild-channel', params: { id: guildId.value, cid: channelId } })
}

// The scroll container below is a persistent DOM node reused across
// channel switches (#241 — no remount per channel anymore), so its
// scrollTop from the previous channel would otherwise carry over. Reset
// it whenever the selected channel changes, matching the old per-channel
// route's remount behavior.
const messageScrollEl = ref<HTMLElement | null>(null)
watch(selectedChannelId, () => {
  nextTick(() => {
    if (messageScrollEl.value) messageScrollEl.value.scrollTop = 0
  })
})

const {
  channel: activeChannel,
  messages,
  authorNames,
  canDelete: canDeleteMessage,
  loading: chatLoading,
  loadingOlder,
  hasMoreOlder,
  error: chatError,
  sendError,
  sending,
  loadOlder,
  sendMessage,
  deleteMessage,
} = useGuildChat(guildId, selectedChannelId)

const draft = ref('')

async function onSendMessage() {
  const body = draft.value
  draft.value = ''
  await sendMessage(body)
}

// Loads the next older page once the reader scrolls near the top of the
// history, rather than a separate "load more" button.
function onMessageScroll(event: Event) {
  const el = event.target as HTMLElement
  if (el.scrollTop < 80 && hasMoreOlder.value && !loadingOlder.value) {
    loadOlder()
  }
}

// --- Game affinity breakdown (issue #206, implementing decision #160) -----
// Aggregated from real GameBinding (#83) data only — never a manager-added
// association (that's #20's superseded associate_game flow below). A
// manage_guild holder always sees it, gated server-side; anyone else only
// once the guild opts into public exposure via the toggle just below.
const gameBreakdownLines = computed(() => {
  if (!gameBreakdown.value) return []
  return gameBreakdown.value.breakdown.map((entry) => formatGameBreakdownEntry(entry, gameBreakdown.value!.total_members))
})
const gameBreakdownEmpty = computed(
  () => gameBreakdown.value !== null && hasNoGameBreakdownData(gameBreakdown.value.breakdown),
)

const savingGameBreakdownPublic = ref(false)
const gameBreakdownPublicError = ref('')

async function onToggleGameBreakdownPublic(next: boolean) {
  if (!session.token) return
  gameBreakdownPublicError.value = ''
  savingGameBreakdownPublic.value = true
  try {
    await api.updateGuild(session.token, guildId.value, { game_breakdown_public: next })
    await refresh()
  } catch (e) {
    gameBreakdownPublicError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingGameBreakdownPublic.value = false
  }
}

// --- Favorite games: curated top-5 pin list (issue #207, implementing -----
// decision #160). Always part of the public profile (`guild.favorite_games`
// — unlike the breakdown above, no public-exposure toggle of its own), so
// it's readable here whether or not the caller can manage the guild.
// Pinning is only ever offered from `gameBreakdown.value.breakdown` — the
// same real-affinity data #206 already gates behind manage_guild — so a
// manager can never even attempt to pin a game without real affinity.
const favorites = computed(() => guild.value?.favorite_games ?? [])
const pinnableGames = computed(() =>
  gameBreakdown.value ? pinnableBreakdownEntries(gameBreakdown.value.breakdown, favorites.value) : [],
)
const canPinMore = computed(() => canPinMoreFavorites(favorites.value))

const savingFavorites = ref(false)
const favoritesError = ref('')

async function applyFavoriteGameIds(gameIds: string[]) {
  if (!session.token) return
  favoritesError.value = ''
  savingFavorites.value = true
  try {
    await api.setFavoriteGames(session.token, guildId.value, gameIds)
    await refresh()
  } catch (e) {
    favoritesError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingFavorites.value = false
  }
}

function onPinFavorite(gameId: string) {
  return applyFavoriteGameIds(addFavoriteGameId(favorites.value, gameId))
}

function onUnpinFavorite(gameId: string) {
  return applyFavoriteGameIds(removeFavoriteGameId(favorites.value, gameId))
}

function onReorderFavorite(gameId: string, direction: 'up' | 'down') {
  return applyFavoriteGameIds(reorderFavoriteGameIds(favorites.value, gameId, direction))
}

// Roster search/filter/sort — milestone-1 polish, plain pure functions
// from api/guilds.ts (mirroring groupMembersByRole's own pattern). Search
// only matches identity id since display names aren't resolvable yet
// (#161). "By role" keeps groupMembersByRole's own ordering; "by name"
// really means "by identity id string" until #161 lands.
const memberQuery = ref('')
const memberSortOrder = ref<MemberSortOrder>('role')
const memberSortOptions = [
  { value: 'role', label: 'By role' },
  { value: 'name', label: 'By identity id' },
]
const visibleMembers = computed(() => {
  const filtered = filterMembersByIdentityId(members.value, memberQuery.value)
  return sortMembers(filtered, memberSortOrder.value)
})
const roleGroups = computed(() => {
  const groups = groupMembersByRole(visibleMembers.value, roles.value)
  // Within each role group, online members before offline — same
  // online-before-offline precedent Friends.vue already applies.
  return groups.map((group) => ({ ...group, members: sortMembersByPresence(group.members) }))
})
const membershipStatus = computed(() => membershipStatusText(isOwner.value, isMember.value))

// "Members currently playing" (#57): realtime presence grouped by game,
// never a durable stat and never phrased as the guild belonging to a game
// (#74 — "N members playing X", not "Game X's guild"). Computed from the
// same roster/presence merge the roles view already loads, so it's always
// null in practice today (no game publishes presence.playing yet — see
// api/guilds.ts's own note) and renders the honest empty state below
// rather than fabricating activity.
const playingGroups = computed(() => groupMembersPlayingByGame(members.value))

// --- Rename / retag / redescribe / MOTD / banner / icon --------------------

const savingField = ref<string | null>(null)
const fieldErrors = ref<Record<string, string>>({})

async function saveGuildField(
  field: 'name' | 'tag' | 'description' | 'motd' | 'banner' | 'icon',
  value: string,
) {
  if (!session.token) return
  fieldErrors.value[field] = ''
  savingField.value = field
  try {
    await api.updateGuild(session.token, guildId.value, { [field]: value })
    await refresh()
  } catch (e) {
    fieldErrors.value[field] = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingField.value = null
  }
}

// Name/tag/description edited together in one modal rather than three
// separate inline fields — a single "Edit" action, one combined
// updateGuild call, closer to how the header actually reads as one unit.
const showEditGuildInfo = ref(false)
const editGuildName = ref('')
const editGuildTag = ref('')
const editGuildDescription = ref('')
const savingGuildInfo = ref(false)
const editGuildInfoError = ref('')

function openEditGuildInfo() {
  if (!guild.value) return
  editGuildName.value = guild.value.name
  editGuildTag.value = guild.value.tag
  editGuildDescription.value = guild.value.description
  editGuildInfoError.value = ''
  showEditGuildInfo.value = true
}

async function onSaveGuildInfo() {
  if (!session.token) return
  editGuildInfoError.value = ''
  savingGuildInfo.value = true
  try {
    await api.updateGuild(session.token, guildId.value, {
      name: editGuildName.value.trim(),
      tag: editGuildTag.value.trim(),
      description: editGuildDescription.value.trim(),
    })
    await refresh()
    showEditGuildInfo.value = false
  } catch (e) {
    editGuildInfoError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingGuildInfo.value = false
  }
}

// --- Recruiting toggle + links (issue #153) -------------------------------

const guildLinks = computed(() => guild.value?.links ?? [])
const savingRecruiting = ref(false)
const recruitingError = ref('')

async function onToggleRecruiting(next: boolean) {
  if (!session.token) return
  recruitingError.value = ''
  savingRecruiting.value = true
  try {
    await api.updateGuild(session.token, guildId.value, { recruiting: next })
    await refresh()
  } catch (e) {
    recruitingError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingRecruiting.value = false
  }
}

const savingJoinPolicy = ref(false)
const joinPolicyError = ref('')

async function onToggleJoinPolicy(next: 'invite_only' | 'open') {
  if (!session.token) return
  joinPolicyError.value = ''
  savingJoinPolicy.value = true
  try {
    await api.updateGuild(session.token, guildId.value, { join_policy: next })
    await refresh()
  } catch (e) {
    joinPolicyError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingJoinPolicy.value = false
  }
}

const newLinkLabel = ref('')
const newLinkUrl = ref('')
const savingLinks = ref(false)
const linksError = ref('')

async function saveLinks(links: { label: string; url: string }[]) {
  if (!session.token) return
  linksError.value = ''
  savingLinks.value = true
  try {
    await api.updateGuild(session.token, guildId.value, { links })
    await refresh()
  } catch (e) {
    linksError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingLinks.value = false
  }
}

async function onAddLink() {
  if (!guild.value || !newLinkLabel.value.trim() || !newLinkUrl.value.trim()) return
  await saveLinks([...guild.value.links, { label: newLinkLabel.value.trim(), url: newLinkUrl.value.trim() }])
  if (!linksError.value) {
    newLinkLabel.value = ''
    newLinkUrl.value = ''
  }
}

async function onRemoveLink(index: number) {
  if (!guild.value) return
  await saveLinks(guild.value.links.filter((_, i) => i !== index))
}

// --- Roles ------------------------------------------------------------

const showAddRole = ref(false)
const newRoleName = ref('')
const newRolePermissions = ref<string[]>([])
const addingRole = ref(false)
const addRoleError = ref('')

function cancelAddRole() {
  showAddRole.value = false
  newRoleName.value = ''
  newRolePermissions.value = []
  addRoleError.value = ''
}

async function onAddRole() {
  if (!session.token) return
  addRoleError.value = ''
  addingRole.value = true
  try {
    await api.createRole(session.token, guildId.value, {
      name: newRoleName.value.trim(),
      permissions: newRolePermissions.value,
    })
    cancelAddRole()
    await refresh()
  } catch (e) {
    addRoleError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    addingRole.value = false
  }
}

// Permission matrix (Role × permission checkboxes) for fast bulk
// assignment on existing roles, alongside the create-role form above.
const togglingPermissionFor = ref<string | null>(null)
const permissionMatrixError = ref('')

async function onTogglePermission(role: RoleResponse, permission: string, event: Event) {
  const checkbox = event.target as HTMLInputElement
  const wasChecked = role.permissions.includes(permission)

  // The owner's permission list is structural, not editable (server-side:
  // update_role rejects any `permissions` change on name_index 0 — the
  // owner's authority comes from guilds.owner, not this row, so it always
  // holds every permission). Reject client-side too, with a clear reason,
  // rather than round-tripping to the server just to find out.
  if (role.name_index === 0) {
    checkbox.checked = wasChecked
    permissionMatrixError.value = "The owner role always has every permission and can't be changed."
    return
  }

  if (!session.token) return
  permissionMatrixError.value = ''
  const key = `${role.name_index}:${permission}`
  togglingPermissionFor.value = key
  const next = wasChecked
    ? role.permissions.filter((p) => p !== permission)
    : [...role.permissions, permission]
  try {
    await api.updateRole(session.token, guildId.value, role.name_index, { permissions: next })
    await refresh()
  } catch (e) {
    // Clicking a checkbox flips its DOM state immediately (native browser
    // behavior, before this handler even runs) — on failure `role.permissions`
    // is unchanged, so Vue's next render sees the same `:checked` value as
    // before and skips re-patching the DOM property, leaving the box stuck
    // showing the failed, not-actually-applied state. Reset it explicitly.
    checkbox.checked = wasChecked
    permissionMatrixError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    togglingPermissionFor.value = null
  }
}

// A row starts locked (plain text name, disabled checkboxes) — clicking
// the pencil unlocks it for both renaming and permission edits together,
// rather than checkboxes always being live to click by accident.
const unlockedRoleIndex = ref<number | null>(null)
const roleNameDraft = ref('')
const renamingRoleFor = ref<number | null>(null)

function unlockRole(role: RoleResponse) {
  unlockedRoleIndex.value = role.name_index
  roleNameDraft.value = role.name
}

function lockRole() {
  unlockedRoleIndex.value = null
}

// Discards any unsaved name draft and re-locks the row. Permission
// checkbox changes have no "draft" to discard — each toggle already
// saved immediately on click — so this only ever affects the name field.
function cancelRoleEdit() {
  unlockedRoleIndex.value = null
  roleNameDraft.value = ''
}

async function onRenameRole(role: RoleResponse) {
  if (!session.token) return
  const name = roleNameDraft.value.trim()
  if (!name || name === role.name) return
  permissionMatrixError.value = ''
  renamingRoleFor.value = role.name_index
  try {
    await api.updateRole(session.token, guildId.value, role.name_index, { name })
    await refresh()
  } catch (e) {
    permissionMatrixError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    renamingRoleFor.value = null
  }
}

// Owner (0) and member (2) are structural — the server always rejects
// deleting either — so the delete action isn't even offered for them.
const BASE_ROLE_INDEXES = [0, 2]
function isBaseRole(role: RoleResponse): boolean {
  return BASE_ROLE_INDEXES.includes(role.name_index)
}

const deletingRoleFor = ref<number | null>(null)

async function onDeleteRole(role: RoleResponse) {
  if (!session.token) return
  permissionMatrixError.value = ''
  deletingRoleFor.value = role.name_index
  try {
    await api.deleteRole(session.token, guildId.value, role.name_index)
    if (unlockedRoleIndex.value === role.name_index) unlockedRoleIndex.value = null
    await refresh()
  } catch (e) {
    permissionMatrixError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    deletingRoleFor.value = null
  }
}

// --- Members: change role / kick ---------------------------------------

const changingRoleFor = ref<string | null>(null)
const roleChangeValue = ref(0)
const changingRole = ref(false)
const changeRoleError = ref('')

function startChangeRole(identityId: string, currentRoleIndex: number) {
  changingRoleFor.value = identityId
  roleChangeValue.value = currentRoleIndex
  changeRoleError.value = ''
}

function cancelChangeRole() {
  changingRoleFor.value = null
  changeRoleError.value = ''
}

async function onSaveRoleChange() {
  if (!session.token || !changingRoleFor.value) return
  changeRoleError.value = ''
  changingRole.value = true
  try {
    await api.updateMemberRole(session.token, guildId.value, changingRoleFor.value, {
      role_index: roleChangeValue.value,
    })
    changingRoleFor.value = null
    await refresh()
  } catch (e) {
    changeRoleError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    changingRole.value = false
  }
}

async function onKick(identityId: string) {
  if (!session.token) return
  actionError.value = ''
  try {
    await api.removeMember(session.token, guildId.value, identityId)
    await refresh()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

// --- Join requests (issue #242) ------------------------------------------
// `joinRequests` (pending-only, per useGuildDetail's default listJoinRequests
// call) is manage_members-gated server-side — empty here for anyone who
// isn't a manager, same non-fatal-403 posture gameBreakdown already has.

const decidingRequestId = ref<string | null>(null)
const joinRequestsError = ref('')

async function onApproveJoinRequest(requestId: string) {
  if (!session.token) return
  joinRequestsError.value = ''
  decidingRequestId.value = requestId
  try {
    await api.approveJoinRequest(session.token, guildId.value, requestId)
    await refresh()
  } catch (e) {
    joinRequestsError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    decidingRequestId.value = null
  }
}

async function onRejectJoinRequest(requestId: string) {
  if (!session.token) return
  joinRequestsError.value = ''
  decidingRequestId.value = requestId
  try {
    await api.rejectJoinRequest(session.token, guildId.value, requestId)
    await refresh()
  } catch (e) {
    joinRequestsError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    decidingRequestId.value = null
  }
}

// Issue #256: the applicant's own view of their application to *this*
// guild — `myJoinRequest` (GET .../join-requests/mine) is self-scoped, so
// unlike `joinRequests` above it's populated for every caller, not just
// managers. Applying reuses the same `create_join_request` endpoint
// Guilds.vue's Discover tab already calls; withdrawing finally exercises
// `withdrawJoinRequest`, present in client.ts since #242 but never wired to
// anything until now.
const myJoinRequestError = ref('')
const applyingToJoin = ref(false)
const withdrawingJoinRequest = ref(false)

async function onApplyToJoin() {
  if (!session.token) return
  myJoinRequestError.value = ''
  applyingToJoin.value = true
  try {
    await api.createJoinRequest(session.token, guildId.value, {})
    await refresh()
  } catch (e) {
    myJoinRequestError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    applyingToJoin.value = false
  }
}

async function onWithdrawJoinRequest() {
  if (!session.token || !myJoinRequest.value) return
  myJoinRequestError.value = ''
  withdrawingJoinRequest.value = true
  try {
    await api.withdrawJoinRequest(session.token, guildId.value, myJoinRequest.value.id)
    await refresh()
  } catch (e) {
    myJoinRequestError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    withdrawingJoinRequest.value = false
  }
}

// --- Invite / join / leave / transfer -----------------------------------

const showInvite = ref(false)
const inviteIdentityId = ref('')
const inviting = ref(false)
const inviteError = ref('')
const inviteSuccessId = ref('')

function cancelInvite() {
  showInvite.value = false
  inviteIdentityId.value = ''
  inviteError.value = ''
}

async function onInvite() {
  if (!session.token) return
  inviteError.value = ''
  inviteSuccessId.value = ''
  inviting.value = true
  try {
    const invite = await api.createGuildInvite(session.token, guildId.value, {
      to: inviteIdentityId.value.trim(),
    })
    // No endpoint lists a player's own pending guild invites yet (a real
    // gap — see docs/architecture/guilds.md's correction note), so the
    // invite id has to be shared with the invitee out of band for them to
    // accept it today.
    inviteSuccessId.value = invite.id
    inviteIdentityId.value = ''
  } catch (e) {
    inviteError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    inviting.value = false
  }
}

async function onJoin() {
  if (!session.token) return
  actionError.value = ''
  try {
    await api.joinGuild(session.token, guildId.value)
    await refresh()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

async function onLeave() {
  if (!session.token) return
  actionError.value = ''
  try {
    await api.leaveGuild(session.token, guildId.value)
    router.push({ name: 'guilds' })
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

const showTransfer = ref(false)
const transferTo = ref('')
const transferring = ref(false)
const transferError = ref('')

function cancelTransfer() {
  showTransfer.value = false
  transferTo.value = ''
  transferError.value = ''
}

async function onTransferOwnership() {
  if (!session.token) return
  transferError.value = ''
  transferring.value = true
  try {
    await api.transferOwnership(session.token, guildId.value, { to: transferTo.value.trim() })
    cancelTransfer()
    await refresh()
  } catch (e) {
    transferError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    transferring.value = false
  }
}

// --- Associate game -------------------------------------------------------
// No game registry/picker exists yet (#18's own deferral) — a plain
// game-id text input is milestone-1 scope, matching how "Add friend" took
// a raw identity id with no search.

const showAssociateGame = ref(false)
const associateGameId = ref('')
const associatingGame = ref(false)
const associateGameError = ref('')

function cancelAssociateGame() {
  showAssociateGame.value = false
  associateGameId.value = ''
  associateGameError.value = ''
}

async function onAssociateGame() {
  if (!session.token) return
  associateGameError.value = ''
  associatingGame.value = true
  try {
    await api.associateGame(session.token, guildId.value, associateGameId.value.trim())
    cancelAssociateGame()
    await refresh()
  } catch (e) {
    associateGameError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    associatingGame.value = false
  }
}

// --- Channel management (create/archive) -------------------------------

const showCreateChannel = ref(false)
const newChannelName = ref('')
const creatingChannel = ref(false)
const createChannelError = ref('')

function cancelCreateChannel() {
  showCreateChannel.value = false
  newChannelName.value = ''
  createChannelError.value = ''
}

async function onCreateChannel() {
  if (!session.token) return
  createChannelError.value = ''
  creatingChannel.value = true
  try {
    await api.createChannel(session.token, guildId.value, { name: newChannelName.value.trim() })
    cancelCreateChannel()
    await refresh()
  } catch (e) {
    createChannelError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    creatingChannel.value = false
  }
}

async function onArchiveChannel(channelId: string) {
  if (!session.token) return
  actionError.value = ''
  try {
    await api.archiveChannel(session.token, guildId.value, channelId)
    await refresh()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

// --- Events (issue #169) -----------------------------------------------

const sortedEvents = computed(() => sortByStartsAt(events.value))

// --- Calendar tab: a navigable month view of the same events list above,
// grouped by local calendar day (localDateKey — see its own doc comment
// on why "local," not the raw UTC starts_at). No separate fetch: the
// guild's full event list is already loaded for the Events tab.
const today = new Date()
const calendarYear = ref(today.getFullYear())
const calendarMonth = ref(today.getMonth() + 1)
const calendarSelectedDate = ref<string | null>(null)

const calendarEventDates = computed(() => events.value.map((e) => localDateKey(e.starts_at)))

const calendarSelectedEvents = computed(() => {
  if (!calendarSelectedDate.value) return []
  return sortedEvents.value.filter((e) => localDateKey(e.starts_at) === calendarSelectedDate.value)
})

function onSelectCalendarDate(date: string) {
  calendarSelectedDate.value = calendarSelectedDate.value === date ? null : date
}

const showCreateEvent = ref(false)
const newEventTitle = ref('')
const newEventDescription = ref('')
const newEventStartsAt = ref('')
const newEventEndsAt = ref('')
const creatingEvent = ref(false)
const createEventError = ref('')

function cancelCreateEvent() {
  showCreateEvent.value = false
  newEventTitle.value = ''
  newEventDescription.value = ''
  newEventStartsAt.value = ''
  newEventEndsAt.value = ''
  createEventError.value = ''
}

async function onCreateEvent() {
  if (!session.token) return
  createEventError.value = ''
  const startsAtIso = newEventStartsAt.value ? new Date(newEventStartsAt.value).toISOString() : ''
  const endsAtIso = newEventEndsAt.value ? new Date(newEventEndsAt.value).toISOString() : ''
  const validation = validateEventForm({
    title: newEventTitle.value,
    description: newEventDescription.value,
    startsAt: startsAtIso,
    endsAt: endsAtIso || undefined,
  })
  if (!validation.valid) {
    createEventError.value = validation.titleError ?? validation.timeRangeError ?? 'Invalid event.'
    return
  }
  creatingEvent.value = true
  try {
    await api.createEvent(session.token, guildId.value, {
      title: newEventTitle.value.trim(),
      description: newEventDescription.value.trim() || undefined,
      starts_at: startsAtIso,
      ends_at: endsAtIso || undefined,
    })
    cancelCreateEvent()
    await refresh()
  } catch (e) {
    createEventError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    creatingEvent.value = false
  }
}

// Self-service only, always the caller's own RSVP — see
// AvalonRsvpControl.types.ts and guild_events.rs::upsert_rsvp.
async function onRsvp(eventId: string, status: 'going' | 'maybe' | 'not_going') {
  if (!session.token) return
  actionError.value = ''
  try {
    await api.rsvpToEvent(session.token, guildId.value, eventId, { status })
    await refresh()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

// Per-member RSVP roster panel (issue #248), shared between the Events tab
// and the Calendar tab's selected-day list below — one instance, opened by
// clicking either tab's event card. Destructured to top-level refs, same
// convention useGuildChat's call site uses, so the template gets plain
// auto-unwrapped bindings instead of `rsvpRoster.foo.value` everywhere.
const {
  open: rsvpRosterOpen,
  loading: rsvpRosterLoading,
  error: rsvpRosterError,
  eventTitle: rsvpRosterEventTitle,
  groups: rsvpRosterGroups,
  openFor: openRsvpRoster,
  close: closeRsvpRoster,
} = useRsvpRoster(guildId)
</script>

<template>
  <div v-if="!loading && guild" :class="styles.page">
    <img v-if="guild.banner" :src="guild.banner" alt="" :class="local.banner" />

    <header :class="[styles.pageHeader, local.headerRow]">
      <div :class="local.titleBlock">
        <div :class="local.titleRow">
          <img v-if="guild.icon" :src="guild.icon" :alt="`${guild.name} icon`" :class="local.headerIcon" />
          <h1 :class="styles.title">{{ guild.name }}</h1>
          <span :class="local.tagBadge">{{ guild.tag }}</span>
        </div>
        <p v-if="guild.description" :class="styles.subtitle">{{ guild.description }}</p>
        <p :class="styles.subtitle">
          {{ guild.member_count }} member{{ guild.member_count === 1 ? '' : 's' }} ·
          {{ guild.join_policy === 'open' ? 'Open to join' : 'Invite only' }}
        </p>
      </div>
    </header>

    <AvalonModal
      title="Edit guild info"
      :open="showEditGuildInfo"
      @close="showEditGuildInfo = false"
    >
      <AvalonForm
        submit-label="Save"
        :submitting="savingGuildInfo"
        :error="editGuildInfoError"
        @submit="onSaveGuildInfo"
      >
        <AvalonTextField v-model="editGuildName" label="Name" />
        <AvalonTextField v-model="editGuildTag" label="Tag (2-5 characters)" :maxlength="5" />
        <AvalonTextField v-model="editGuildDescription" label="Description" />
        <template #secondary-actions>
          <AvalonButton label="Cancel" variant="secondary" @click="showEditGuildInfo = false" />
        </template>
      </AvalonForm>
    </AvalonModal>

    <p v-if="error" :class="styles.error">{{ error }}</p>
    <p v-if="actionError" :class="styles.error">{{ actionError }}</p>

    <div :class="local.tabs">
      <button
        v-for="tab in TABS"
        :key="tab.key"
        type="button"
        :class="[local.tab, activeTab === tab.key && local.tabActive]"
        @click="selectTab(tab.key)"
      >
        {{ tab.label }}
      </button>
    </div>

    <!-- Overview: header info already above, plus MOTD/banner/links, game
         affinity, favorite games, associated games, and guild history —
         all read-only here; editing lives in Settings. -->
    <div v-if="activeTab === 'overview'" :class="styles.grid">
      <div :class="styles.mainColumn">
        <AvalonCard v-if="guild.motd || guildLinks.length > 0" title="About">
          <p v-if="guild.motd" :class="styles.subtitle">{{ guild.motd }}</p>
          <p v-for="link in guildLinks" :key="link.url" :class="styles.empty">
            <a :href="link.url" target="_blank" rel="noopener noreferrer">{{ link.label }}</a>
          </p>
        </AvalonCard>

        <AvalonCard
          v-if="canManageGuild || gameBreakdown"
          title="Game affinity"
          subtitle="Auto-derived from members' active game bindings — not something anyone sets by hand. Managers can choose whether it's visible on this guild's public profile and discovery card; it's always visible to members."
        >
          <p v-if="canManageGuild" :class="styles.empty">
            Shown on this guild's public profile and discovery card:
            {{ guild.game_breakdown_public ? 'yes' : 'no' }}
          </p>
          <AvalonButton
            v-if="canManageGuild"
            :label="
              savingGameBreakdownPublic
                ? 'Saving…'
                : guild.game_breakdown_public
                  ? 'Hide from public profile'
                  : 'Show on public profile'
            "
            variant="secondary"
            @click="onToggleGameBreakdownPublic(!guild.game_breakdown_public)"
          />
          <p v-if="gameBreakdownPublicError" :class="styles.error">{{ gameBreakdownPublicError }}</p>

          <p v-if="gameBreakdownError && !canManageGuild" :class="styles.empty">
            This guild hasn't shared its game affinity breakdown publicly.
          </p>
          <template v-else>
            <p v-for="line in gameBreakdownLines" :key="line" :class="styles.empty">{{ line }}</p>
            <p v-if="gameBreakdownEmpty" :class="styles.empty">
              No guild member has an active game binding yet.
            </p>
          </template>
        </AvalonCard>

        <AvalonCard v-if="canManageGuild || favorites.length > 0" title="Favorite games">
          <p v-if="favorites.length === 0" :class="styles.empty">No favorite games pinned yet.</p>
          <div v-for="(entry, index) in favorites" :key="entry.game_id" :class="styles.empty">
            {{ formatFavoriteGameEntry(entry) }}
            <template v-if="canManageGuild">
              <AvalonButton
                v-if="index > 0"
                label="Move up"
                variant="secondary"
                :disabled="savingFavorites"
                @click="onReorderFavorite(entry.game_id, 'up')"
              />
              <AvalonButton
                v-if="index < favorites.length - 1"
                label="Move down"
                variant="secondary"
                :disabled="savingFavorites"
                @click="onReorderFavorite(entry.game_id, 'down')"
              />
              <AvalonButton
                label="Unpin"
                variant="danger"
                :disabled="savingFavorites"
                @click="onUnpinFavorite(entry.game_id)"
              />
            </template>
          </div>

          <template v-if="canManageGuild">
            <p v-if="!canPinMore" :class="styles.empty">Up to 5 games may be pinned at once.</p>
            <p v-else-if="pinnableGames.length === 0" :class="styles.empty">
              No unpinned game currently has affinity to pin.
            </p>
            <div v-for="entry in pinnableGames" :key="entry.game_id" :class="styles.empty">
              {{ entry.game_name }}
              <AvalonButton
                label="Pin"
                variant="secondary"
                :disabled="savingFavorites"
                @click="onPinFavorite(entry.game_id)"
              />
            </div>
            <p v-if="favoritesError" :class="styles.error">{{ favoritesError }}</p>
          </template>
        </AvalonCard>

        <!--
          Associated games (#20's original manual associate_game flow,
          superseded by #206/#207's real-binding-derived affinity above but
          still live): now visible to any member, read-only — only the
          "associate a game" action itself stays canManageGuild-gated
          (issue #241).
        -->
        <AvalonCard v-if="canManageGuild || guild.games.length > 0" title="Associated games">
          <p v-for="gameId in guild.games" :key="gameId" :class="styles.empty">{{ gameId }}</p>
          <p v-if="guild.games.length === 0" :class="styles.empty">No games associated yet.</p>
          <template v-if="canManageGuild">
            <AvalonButton
              v-show="!showAssociateGame"
              label="Associate a game"
              variant="secondary"
              @click="showAssociateGame = true"
            />
            <div v-show="showAssociateGame">
              <AvalonForm
                submit-label="Associate"
                :submitting="associatingGame"
                :error="associateGameError"
                @submit="onAssociateGame"
              >
                <AvalonTextField v-model="associateGameId" label="Game id" />
                <template #secondary-actions>
                  <AvalonButton label="Cancel" variant="secondary" @click="cancelAssociateGame" />
                </template>
              </AvalonForm>
            </div>
          </template>
        </AvalonCard>

        <!--
          Guild history (created / member joined / left / role changed) is
          durable per docs/architecture/guilds.md, but #82's event catalogue
          and indexer exposure for it isn't necessarily done, and no
          endpoint like GET /guilds/:id/history exists today (see
          docs/architecture/hub.md's endpoint list). Rather than fabricate a
          history feed from the current roster/role snapshot, this section
          says plainly that it isn't available yet.
        -->
        <AvalonCard title="History">
          <p :class="styles.empty">
            History isn't available yet — this guild's events (created, joined, left, role
            changed) are recorded on the network, but no view of them is exposed here until the
            event catalogue and its indexer projection land.
          </p>
        </AvalonCard>
      </div>

      <div :class="styles.sideColumn">
        <AvalonCard title="Membership">
          <p :class="styles.empty">{{ membershipStatus }}</p>
          <AvalonButton
            v-if="guild.join_policy === 'open' && !isMember"
            label="Join guild"
            variant="primary"
            @click="onJoin"
          />
          <!--
            Issue #256: invite-only guilds have no direct "Join guild"
            button above — the applicant path is apply-then-approve. `mine`
            (myJoinRequest) tells this specific guild page whether the
            caller already has one pending, so it can show a withdraw
            action instead of a second "Apply to join" that would just
            return the same pending row (create_join_request is idempotent,
            but this reads better).
          -->
          <template v-if="guild.join_policy !== 'open' && !isMember">
            <template v-if="myJoinRequest">
              <p :class="styles.empty">
                Application pending<template v-if="myJoinRequest.message">
                  — "{{ myJoinRequest.message }}"</template
                >.
              </p>
              <AvalonButton
                :label="withdrawingJoinRequest ? 'Withdrawing…' : 'Withdraw request'"
                variant="danger"
                :disabled="withdrawingJoinRequest"
                @click="onWithdrawJoinRequest"
              />
            </template>
            <AvalonButton
              v-else-if="guild.recruiting"
              :label="applyingToJoin ? 'Applying…' : 'Apply to join'"
              variant="secondary"
              :disabled="applyingToJoin"
              @click="onApplyToJoin"
            />
          </template>
          <AvalonButton v-if="isMember && !isOwner" label="Leave guild" variant="danger" @click="onLeave" />
          <p v-if="myJoinRequestError" :class="styles.error">{{ myJoinRequestError }}</p>
        </AvalonCard>
      </div>
    </div>

    <!-- Members: roster (search/sort/role management), currently-playing
         summary, and invite. -->
    <div v-else-if="activeTab === 'members'" :class="styles.grid">
      <div :class="styles.mainColumn">
        <AvalonCard title="Members">
          <AvalonFilterBar
            label="Search by identity id"
            placeholder="identity:ab12…"
            :query="memberQuery"
            :sort-options="memberSortOptions"
            :sort-value="memberSortOrder"
            @update:query="memberQuery = $event"
            @update:sort-value="memberSortOrder = $event as MemberSortOrder"
          />
          <p v-if="memberQuery && visibleMembers.length === 0" :class="styles.empty">
            No members match "{{ memberQuery }}".
          </p>
          <div v-for="group in roleGroups" :key="group.roleIndex">
            <p :class="styles.empty">{{ group.roleName }} — {{ group.members.length }}</p>
            <AvalonGuildMemberRow
              v-for="member in group.members"
              :key="member.identityId"
              :identity-id="member.identityId"
              :display-name="member.displayName"
              :status="member.status"
              :role-name="group.roleName"
              :role-variant="roleVariantForIndex(member.roleIndex)"
              :can-change-role="canChangeMemberRole(guild, selfId, selfPermissions, member)"
              :can-kick="canKickMember(guild, selfId, selfPermissions, member)"
              @change-role="startChangeRole(member.identityId, member.roleIndex)"
              @kick="onKick(member.identityId)"
            />
          </div>

          <div v-if="changingRoleFor">
            <AvalonForm
              submit-label="Save role"
              :submitting="changingRole"
              :error="changeRoleError"
              @submit="onSaveRoleChange"
            >
              <label :class="styles.subtitle">
                New role
                <select v-model.number="roleChangeValue">
                  <option v-for="role in roles" :key="role.name_index" :value="role.name_index">
                    {{ role.name }}
                  </option>
                </select>
              </label>
              <template #secondary-actions>
                <AvalonButton label="Cancel" variant="secondary" @click="cancelChangeRole" />
              </template>
            </AvalonForm>
          </div>
        </AvalonCard>
      </div>

      <div :class="styles.sideColumn">
        <AvalonCard title="Currently playing">
          <p v-if="playingGroups.length === 0" :class="styles.empty">
            No members currently reporting an in-game presence.
          </p>
          <template v-else>
            <p v-for="group in playingGroups" :key="group.gameId" :class="styles.empty">
              {{ formatPlayingSummary(group) }}
            </p>
          </template>
          <p :class="styles.empty">Live presence, not a durable stat — updates as members' status changes.</p>
        </AvalonCard>

        <AvalonCard v-if="canManageMembers" title="Invite a player">
          <p v-if="inviteSuccessId" :class="styles.empty">
            Invite sent (id {{ inviteSuccessId }}) — share it with them to accept.
          </p>
          <AvalonButton
            v-show="!showInvite"
            label="Invite a player"
            variant="secondary"
            @click="showInvite = true"
          />
          <div v-show="showInvite">
            <AvalonForm
              submit-label="Send invite"
              :submitting="inviting"
              :error="inviteError"
              @submit="onInvite"
            >
              <AvalonTextField v-model="inviteIdentityId" label="Identity id" />
              <template #secondary-actions>
                <AvalonButton label="Cancel" variant="secondary" @click="cancelInvite" />
              </template>
            </AvalonForm>
          </div>
        </AvalonCard>

        <!--
          Issue #242: applicant-initiated join requests, the counterpart to
          "Invite a player" above. manage_members-gated same as that card
          (the server independently enforces this — joinRequests is simply
          empty for anyone else). Pending only, matching
          crates/server/src/guilds.rs::list_join_requests' own default.
        -->
        <AvalonCard v-if="canManageMembers" title="Applications">
          <p v-if="joinRequests.length === 0" :class="styles.empty">No pending applications.</p>
          <div v-for="request in joinRequests" :key="request.id" :class="styles.empty">
            {{ request.applicant }}
            <template v-if="request.message">— "{{ request.message }}"</template>
            <AvalonButton
              label="Approve"
              variant="primary"
              :disabled="decidingRequestId === request.id"
              @click="onApproveJoinRequest(request.id)"
            />
            <AvalonButton
              label="Reject"
              variant="danger"
              :disabled="decidingRequestId === request.id"
              @click="onRejectJoinRequest(request.id)"
            />
          </div>
          <p v-if="joinRequestsError" :class="styles.error">{{ joinRequestsError }}</p>
        </AvalonCard>
      </div>
    </div>

    <!-- Channels (issue #241): a persistent sidebar of channels next to the
         active channel's messages — switching channels updates
         `selectedChannelId` and lets useGuildChat reload in place, never a
         route navigation or component remount. -->
    <div v-else-if="activeTab === 'channels'" :class="local.channelsLayout">
      <div :class="local.channelSidebar">
        <AvalonCard title="Channels">
          <AvalonChannelList
            :channels="channels"
            :active-channel-id="selectedChannelId"
            :can-manage="canManageChannels"
            @select="selectChannel"
            @create="showCreateChannel = true"
            @archive="onArchiveChannel"
          />
          <div v-if="showCreateChannel">
            <AvalonForm
              submit-label="Create channel"
              :submitting="creatingChannel"
              :error="createChannelError"
              @submit="onCreateChannel"
            >
              <AvalonTextField v-model="newChannelName" label="Channel name" placeholder="general" />
              <template #secondary-actions>
                <AvalonButton label="Cancel" variant="secondary" @click="cancelCreateChannel" />
              </template>
            </AvalonForm>
          </div>
        </AvalonCard>
      </div>

      <div :class="local.channelMain">
        <p v-if="channels.length === 0" :class="styles.empty">
          No channels yet — {{ canManageChannels ? 'create one to start chatting.' : 'nothing to read yet.' }}
        </p>
        <template v-else>
          <p v-if="chatError" :class="styles.error">{{ chatError }}</p>
          <p v-if="chatLoading" :class="styles.empty">Loading channel…</p>
          <AvalonCard v-else :title="activeChannel ? `#${activeChannel.name}` : 'channel'">
            <p v-if="activeChannel?.archived" :class="styles.subtitle">
              This channel is archived — history is readable, but new messages can't be sent.
            </p>
            <p :class="styles.empty">
              Message history is subject to the server's retention policy, not permanent.
            </p>
            <div ref="messageScrollEl" :class="local.messageScroll" @scroll="onMessageScroll">
              <p v-if="loadingOlder" :class="styles.empty">Loading older messages…</p>
              <p v-else-if="!hasMoreOlder && messages.length > 0" :class="styles.empty">
                Start of channel history.
              </p>
              <p v-if="messages.length === 0" :class="styles.empty">No messages yet — say hello.</p>
              <AvalonChatMessage
                v-for="message in messages"
                :key="message.id"
                :author-id="message.author"
                :author-display-name="authorNames[message.author]"
                :body="message.body"
                :sent-at-label="new Date(message.sent_at).toLocaleString()"
                :can-delete="canDeleteMessage"
                @delete="deleteMessage(message.id)"
              />
            </div>

            <AvalonChatComposer
              v-model="draft"
              :max-chars="MESSAGE_BODY_MAX_CHARS"
              :sending="sending"
              :error="sendError"
              :disabled="activeChannel?.archived ?? false"
              @send="onSendMessage"
            />
          </AvalonCard>

          <!-- Per-resource permission overrides (issue #250): announcement-only
               toggle plus per-role channel_post/manage_channels overrides for
               the active channel. Visible to anyone who can manage channels
               and/or roles — server re-checks each action independently. -->
          <ChannelPermissionOverrides
            v-if="activeChannel && (canManageChannels || canManageRoles)"
            :token="session.token ?? ''"
            :guild-id="guildId"
            :channel="activeChannel"
            :roles="roles"
            :can-manage-roles="canManageRoles"
            :can-manage-channels="canManageChannels"
            @updated="refresh"
          />
        </template>
      </div>
    </div>

    <!--
      Guild events calendar + RSVP (issue #169). Neither an event nor
      an RSVP row is durable protocol history — see
      docs/architecture/guilds.md's "Guild events calendar + RSVP"
      section — so nothing here claims to be permanent. The list
      endpoint doesn't currently return the *caller's own* RSVP status
      per event (only aggregate counts), so AvalonRsvpControl always
      renders with no pre-selected status today — a real gap, not
      silently worked around.
    -->
    <div v-else-if="activeTab === 'events'" :class="styles.mainColumn">
        <AvalonCard title="Events">
          <p v-if="sortedEvents.length === 0" :class="styles.empty">No upcoming events yet.</p>
          <div
            v-for="event in sortedEvents"
            :key="event.id"
            :class="local.eventCardClickable"
            @click="openRsvpRoster(event.id, event.title)"
          >
            <AvalonEventCard
              :title="event.title"
              :description="event.description ?? undefined"
              :starts-at="event.starts_at"
              :ends-at="event.ends_at ?? undefined"
              :rsvp-counts="event.rsvp_counts"
            >
              <template #actions>
                <div @click.stop>
                  <AvalonRsvpControl @rsvp="(status) => onRsvp(event.id, status)" />
                </div>
              </template>
            </AvalonEventCard>
          </div>
          <AvalonButton
            v-if="canManageChannels && !showCreateEvent"
            label="+ New event"
            variant="secondary"
            @click="showCreateEvent = true"
          />
          <div v-if="showCreateEvent">
            <AvalonForm
              submit-label="Create event"
              :submitting="creatingEvent"
              :error="createEventError"
              @submit="onCreateEvent"
            >
              <AvalonTextField v-model="newEventTitle" label="Title" placeholder="Raid night" />
              <AvalonTextField
                v-model="newEventDescription"
                label="Description"
                placeholder="Optional details"
              />
              <AvalonDateTimeField v-model="newEventStartsAt" label="Starts at" />
              <AvalonDateTimeField v-model="newEventEndsAt" label="Ends at (optional)" />
              <template #secondary-actions>
                <AvalonButton label="Cancel" variant="secondary" @click="cancelCreateEvent" />
              </template>
            </AvalonForm>
          </div>
        </AvalonCard>
    </div>

    <!-- Calendar: same event list as the Events tab above, grouped onto a
         navigable month grid with a dot under any day that has an event —
         click a day to see what's on it below the calendar. Full width,
         not the two-column grid the other tabs use — a month grid reads
         better wide, and there's nothing to put in a side column here. -->
    <div v-else-if="activeTab === 'calendar'" :class="styles.mainColumn">
        <AvalonCard title="Calendar">
          <AvalonCalendarMonth
            :year="calendarYear"
            :month="calendarMonth"
            :event-dates="calendarEventDates"
            :selected-date="calendarSelectedDate"
            @update:year="calendarYear = $event"
            @update:month="calendarMonth = $event"
            @select-date="onSelectCalendarDate"
          />

          <template v-if="calendarSelectedDate">
            <p :class="styles.empty">{{ calendarSelectedDate }}</p>
            <p v-if="calendarSelectedEvents.length === 0" :class="styles.empty">
              No events on this day.
            </p>
            <div
              v-for="event in calendarSelectedEvents"
              :key="event.id"
              :class="local.eventCardClickable"
              @click="openRsvpRoster(event.id, event.title)"
            >
              <AvalonEventCard
                :title="event.title"
                :description="event.description ?? undefined"
                :starts-at="event.starts_at"
                :ends-at="event.ends_at ?? undefined"
                :rsvp-counts="event.rsvp_counts"
              >
                <template #actions>
                  <div @click.stop>
                    <AvalonRsvpControl @rsvp="(status) => onRsvp(event.id, status)" />
                  </div>
                </template>
              </AvalonEventCard>
            </div>
          </template>
        </AvalonCard>
    </div>

    <!-- Roles: read-only list for any member, add-role form canManageRoles-gated
         (unchanged permission behavior from before #241, just relocated). -->
    <div v-else-if="activeTab === 'roles'" :class="styles.mainColumn">
        <AvalonCard
          v-if="canManageRoles"
          title="Roles"
          subtitle="Click the pencil to unlock a role for renaming and permission changes."
        >
          <p v-if="permissionMatrixError" :class="styles.error">{{ permissionMatrixError }}</p>
          <div :class="local.permissionMatrixScroll">
            <table :class="local.permissionMatrix">
              <thead>
                <tr>
                  <th>Role</th>
                  <th v-for="permission in PERMISSION_OPTIONS" :key="permission">{{ permission }}</th>
                  <th></th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="role in roles" :key="role.name_index">
                  <td>
                    <div :class="local.roleNameCell">
                      <button
                        type="button"
                        :class="local.roleLockButton"
                        :aria-label="unlockedRoleIndex === role.name_index ? 'Lock role' : 'Unlock role to edit'"
                        @click="
                          unlockedRoleIndex === role.name_index
                            ? lockRole()
                            : unlockRole(role)
                        "
                      >
                        <AvalonIcon :name="unlockedRoleIndex === role.name_index ? 'check' : 'pencil'" :size="14" />
                      </button>
                      <button
                        v-if="unlockedRoleIndex === role.name_index"
                        type="button"
                        :class="local.roleLockButton"
                        aria-label="Cancel editing"
                        @click="cancelRoleEdit"
                      >
                        <AvalonIcon name="close" :size="14" />
                      </button>
                      <input
                        v-if="unlockedRoleIndex === role.name_index"
                        v-model="roleNameDraft"
                        :class="local.roleNameInput"
                        type="text"
                        :disabled="renamingRoleFor === role.name_index"
                        @blur="onRenameRole(role)"
                        @keydown.enter="onRenameRole(role)"
                      />
                      <span v-else>{{ role.name }}</span>
                    </div>
                  </td>
                  <td v-for="permission in PERMISSION_OPTIONS" :key="permission">
                    <input
                      type="checkbox"
                      :checked="role.permissions.includes(permission)"
                      :disabled="
                        unlockedRoleIndex !== role.name_index ||
                        togglingPermissionFor === `${role.name_index}:${permission}`
                      "
                      @change="onTogglePermission(role, permission, $event)"
                    />
                  </td>
                  <td>
                    <AvalonButton
                      v-if="unlockedRoleIndex === role.name_index && !isBaseRole(role)"
                      :label="deletingRoleFor === role.name_index ? 'Deleting…' : 'Delete'"
                      variant="danger"
                      :disabled="deletingRoleFor === role.name_index"
                      @click="onDeleteRole(role)"
                    />
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
          <AvalonButton
            v-show="!showAddRole"
            label="Define a role"
            variant="secondary"
            @click="showAddRole = true"
          />
          <div v-show="showAddRole">
            <AvalonForm
              submit-label="Create role"
              :submitting="addingRole"
              :error="addRoleError"
              @submit="onAddRole"
            >
              <AvalonTextField v-model="newRoleName" label="Role name" placeholder="raid leader" />
              <div v-for="permission in PERMISSION_OPTIONS" :key="permission">
                <label>
                  <input type="checkbox" :value="permission" v-model="newRolePermissions" />
                  {{ permission }}
                </label>
              </div>
              <template #secondary-actions>
                <AvalonButton label="Cancel" variant="secondary" @click="cancelAddRole" />
              </template>
            </AvalonForm>
          </div>
        </AvalonCard>
        <p v-else :class="styles.empty">You don't have permission to manage this guild's roles.</p>
    </div>

    <!-- Settings: recruiting toggle, MOTD/banner/links editing, transfer
         ownership — canManageGuild-gated, same as every other management
         action on this page. -->
    <div v-else-if="activeTab === 'settings'" :class="styles.grid">
      <template v-if="canManageGuild">
        <div :class="styles.mainColumn">
          <AvalonCard title="Guild info" subtitle="Name, tag, and description.">
            <p :class="styles.empty">{{ guild.name }} [{{ guild.tag }}]</p>
            <p :class="styles.empty">{{ guild.description || 'No description' }}</p>
            <AvalonButton label="Edit" variant="secondary" @click="openEditGuildInfo" />
          </AvalonCard>

          <AvalonCard title="Recruiting">
            <p :class="styles.empty">
              Recruiting guilds are discoverable on the "Discover" board:
              {{ guild.recruiting ? 'yes' : 'no' }}
            </p>
            <AvalonButton
              :label="savingRecruiting ? 'Saving…' : guild.recruiting ? 'Stop recruiting' : 'Start recruiting'"
              variant="secondary"
              @click="onToggleRecruiting(!guild.recruiting)"
            />
            <p v-if="recruitingError" :class="styles.error">{{ recruitingError }}</p>
          </AvalonCard>

          <AvalonCard
            title="Membership"
            subtitle="Separate from recruiting above: this controls whether joining requires an invite or approval at all."
          >
            <p :class="styles.empty">
              {{
                guild.join_policy === 'open'
                  ? 'Open — anyone can join instantly, no invite or approval needed.'
                  : 'Invite only — joining requires an invite, or an application a manager approves.'
              }}
            </p>
            <AvalonButton
              :label="
                savingJoinPolicy
                  ? 'Saving…'
                  : guild.join_policy === 'open'
                    ? 'Switch to invite only'
                    : 'Switch to open'
              "
              variant="secondary"
              @click="onToggleJoinPolicy(guild.join_policy === 'open' ? 'invite_only' : 'open')"
            />
            <p v-if="joinPolicyError" :class="styles.error">{{ joinPolicyError }}</p>
          </AvalonCard>

          <AvalonCard title="Message of the day, banner & icon">
            <AvalonEditableField
              label="MOTD"
              :value="guild.motd ?? ''"
              empty-text="No MOTD set"
              :saving="savingField === 'motd'"
              :error="fieldErrors.motd"
              @save="saveGuildField('motd', $event)"
            />
            <AvalonEditableField
              label="Banner URL"
              :value="guild.banner ?? ''"
              empty-text="No banner set"
              placeholder="https://…"
              :saving="savingField === 'banner'"
              :error="fieldErrors.banner"
              @save="saveGuildField('banner', $event)"
            />
            <AvalonEditableField
              label="Icon URL"
              :value="guild.icon ?? ''"
              empty-text="No icon set"
              placeholder="https://…"
              :saving="savingField === 'icon'"
              :error="fieldErrors.icon"
              @save="saveGuildField('icon', $event)"
            />
          </AvalonCard>

          <AvalonCard title="Links">
            <p v-if="guildLinks.length === 0" :class="styles.empty">No links added yet.</p>
            <div v-for="(link, index) in guildLinks" :key="link.url" :class="local.linkRow">
              <span :class="styles.empty">{{ link.label }} — {{ link.url }}</span>
              <AvalonButton
                label="Remove"
                variant="danger"
                :disabled="savingLinks"
                @click="onRemoveLink(index)"
              />
            </div>
            <div :class="local.addLinkRow">
              <AvalonTextField v-model="newLinkLabel" label="Label" placeholder="Discord" />
              <AvalonTextField v-model="newLinkUrl" label="URL" placeholder="https://discord.gg/…" />
              <AvalonButton label="Add link" variant="secondary" :disabled="savingLinks" @click="onAddLink" />
            </div>
            <p v-if="linksError" :class="styles.error">{{ linksError }}</p>
          </AvalonCard>
        </div>

        <div :class="styles.sideColumn">
          <AvalonCard v-if="isOwner" title="Transfer ownership">
            <AvalonButton
              v-show="!showTransfer"
              label="Transfer ownership"
              variant="danger"
              @click="showTransfer = true"
            />
            <div v-show="showTransfer">
              <AvalonForm
                submit-label="Transfer"
                :submitting="transferring"
                :error="transferError"
                @submit="onTransferOwnership"
              >
                <AvalonTextField v-model="transferTo" label="New owner's identity id" />
                <template #secondary-actions>
                  <AvalonButton label="Cancel" variant="secondary" @click="cancelTransfer" />
                </template>
              </AvalonForm>
            </div>
          </AvalonCard>
        </div>
      </template>
      <p v-else :class="styles.empty">Only this guild's managers can view its settings.</p>
    </div>

    <!-- Per-member RSVP roster (issue #248): shared between the Events tab
         and the Calendar tab's selected-day list above, one panel instance
         opened by clicking either tab's event card. -->
    <AvalonRsvpRosterPanel
      :open="rsvpRosterOpen"
      :event-title="rsvpRosterEventTitle"
      :loading="rsvpRosterLoading"
      :error="rsvpRosterError"
      :groups="rsvpRosterGroups"
      @close="closeRsvpRoster"
    />
  </div>
</template>
