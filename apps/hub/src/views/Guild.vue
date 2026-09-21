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
import type { AvalonIconName } from '@avalon/ui'
import type { Role, GuildEvent } from '@avalon/sdk'
import ResourcePermissionOverrides from '../components/ResourcePermissionOverrides.vue'
import { MESSAGE_BODY_MAX_CHARS } from '../api/guildChat'
import { localDateKey, sortByStartsAt, toLocalDateTimeInput, validateEventForm } from '../api/guildEvents'
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
  groupMembersPlayingByIntegrator,
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
  type RoleBadgeColorId,
  type RoleBadgeIconId,
} from '../api/guilds'
import { useGuildChat } from '../composables/useGuildChat'
import { useGuildDetail } from '../composables/useGuildDetail'
import { useRsvpRoster } from '../composables/useRsvpRoster'
import { useSessionStore } from '../api/session'
import { isIdentityId } from '../utils/identity'
import local from '../styles/Guild.module.scss'
import styles from '../styles/page.module.scss'

// Issue #250 added `event_manage` (split out of `manage_channels`) and
// `channel_post` (the announcement-only-channels proof point) to the base
// GuildPermission vocabulary — both editable here as ordinary base
// permissions, same as the original four. Per-resource overrides on top
// of these are a separate surface (ResourcePermissionOverrides.vue on the
// Channels and Events tabs), not this guild-wide matrix. `view`/
// `view_details` (#458) are deliberately NOT listed here — their
// resolution (`resolve_view_permission`) never even reads a role's base
// permission list, only its per-resource overrides, so a guild-wide base
// grant here would have no effect and would be misleading to show.
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
  integratorBreakdown,
  integratorBreakdownError,
  joinRequests,
  applicantNames,
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
// Issue #463: events have their own `event_manage` permission (#250) —
// previously ungated in the Hub, which fell back to reusing
// canManageChannels for the "+ New event" button. This is still the flat
// (non-resource-aware) check: a plain member granted event_manage on one
// specific event via a per-resource override won't see edit/delete on
// that event either, matching the same limitation canManageChannels
// already has for channel-level overrides.
const canManageEvents = computed(
  () => guild.value !== null && hasGuildPermission(guild.value, selfId.value, selfPermissions.value, 'event_manage'),
)
const isMember = computed(() => members.value.some((m) => m.identityId === selfId.value))

// Author presence for chat messages (issue #438 follow-up) — `members`
// already carries each member's presence via `listMembersWithPresence`
// (useGuildDetail.ts), refreshed on that composable's own poll cadence
// rather than a separate live subscription just for chat.
const memberPresence = computed(() => {
  const map: Record<string, (typeof members.value)[number]['status']> = {}
  for (const member of members.value) {
    map[member.identityId] = member.status
  }
  return map
})

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

// Issue #391: channels are member-only server-side — hidden entirely for a
// non-member. Issue #448 carves events/calendar out of this: a non-member
// of a public guild (guild.public, #449) now sees a read-only Events/
// Calendar tab (server-filtered to that guild's public events), so those
// two are no longer flatly member-only — only channels stays that way.
const MEMBER_ONLY_TABS: TabKey[] = ['channels']
const EVENT_TABS: TabKey[] = ['events', 'calendar']
// Settings is different from the member-only tabs above: it's hidden from
// everyone, members included, unless they can actually act on it
// (manage_guild, or the owner — canManageGuild already covers both) —
// showing an always-"only managers can view this" tab to every member
// isn't useful, it's just a dead end.
const visibleTabs = computed(() =>
  TABS.filter((tab) => {
    if (tab.key === 'settings') return canManageGuild.value
    if (EVENT_TABS.includes(tab.key)) return isMember.value || (guild.value?.public ?? false)
    return isMember.value || !MEMBER_ONLY_TABS.includes(tab.key)
  }),
)

const activeTab = ref<TabKey>(route.name === 'guild-channel' ? 'channels' : 'overview')

// A non-member landed here via a deep link into a member-only tab (e.g.
// `/guilds/:id/channels/:cid`) — once membership is known, fall back to the
// always-visible Overview tab instead of a hidden one. Gated on `loading`
// (rather than `isMember` directly) so this doesn't fire on mount, before
// membership is known, and wrongly bounce an actual member away from a
// deep-linked channel.
watch(loading, (isLoading) => {
  if (!isLoading && !isMember.value && MEMBER_ONLY_TABS.includes(activeTab.value)) {
    activeTab.value = 'overview'
  }
  if (
    !isLoading &&
    !isMember.value &&
    EVENT_TABS.includes(activeTab.value) &&
    !(guild.value?.public ?? false)
  ) {
    activeTab.value = 'overview'
  }
  if (!isLoading && activeTab.value === 'settings' && !canManageGuild.value) {
    activeTab.value = 'overview'
  }
})

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
// channel switches (#241 — no remount per channel anymore).
const messageScrollEl = ref<HTMLElement | null>(null)

// Within this many px of the bottom counts as "at the bottom" for
// auto-scroll purposes — a reader doesn't have to be pixel-perfect at the
// very edge to keep following new messages live.
const NEAR_BOTTOM_THRESHOLD_PX = 120

function isNearBottom(el: HTMLElement): boolean {
  return el.scrollHeight - el.scrollTop - el.clientHeight < NEAR_BOTTOM_THRESHOLD_PX
}

function scrollToBottom() {
  if (messageScrollEl.value) {
    messageScrollEl.value.scrollTop = messageScrollEl.value.scrollHeight
  }
}

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

// Opening a channel (first load or a switch) always lands at the bottom —
// the same behavior a fresh chat window opening to its latest messages
// has everywhere else. Tracked as a plain flag rather than watching
// `chatLoading` directly: `messages` and `loading` both change inside the
// same async `load()` call, and which reactive flush order they land in
// isn't something to depend on — this flag is set synchronously on every
// channel switch and consumed the next time `messages` actually
// repopulates, regardless of flush timing.
let forceScrollOnNextMessages = false
watch(selectedChannelId, () => {
  forceScrollOnNextMessages = true
})

// A message arriving (send or live push) only pulls the view down if the
// reader was already at the bottom — someone scrolled up into history
// keeps reading exactly where they are; this watcher runs before Vue
// patches the DOM for the new message (default 'pre' flush), so
// `messageScrollEl`'s measurements here are still the pre-append ones.
watch(
  () => messages.value.length,
  (newLen, oldLen) => {
    const el = messageScrollEl.value
    const wasNearBottom = !el || isNearBottom(el)
    if (forceScrollOnNextMessages) {
      if (newLen === 0) return // cleared, waiting for the real repopulation
      forceScrollOnNextMessages = false
      nextTick(scrollToBottom)
      return
    }
    if (newLen > oldLen && wasNearBottom) {
      nextTick(scrollToBottom)
    }
  },
)

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

// --- Channel topic + announcement-only (issue #276) -----------------------
// Both live on the active channel itself now, next to its name/messages —
// previously the announcement-only toggle was buried inside
// ResourcePermissionOverrides.vue's role x permission grid, a strange home
// for a simple per-channel setting. `activeChannel` (from useGuildChat) is
// patched in place from each PATCH response rather than waiting on a
// reload, since useGuildChat only refetches channel metadata when
// guildId/channelId themselves change (#241), not on demand.
const savingChannelTopic = ref(false)
const channelTopicError = ref('')

async function onSaveChannelTopic(value: string) {
  const s = session.session
  if (!s || !activeChannel.value) return
  channelTopicError.value = ''
  savingChannelTopic.value = true
  try {
    const updated = await s.updateChannel(guildId.value, activeChannel.value.id, {
      name: activeChannel.value.name,
      topic: value,
    })
    activeChannel.value = updated
    await refresh()
  } catch (e) {
    channelTopicError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingChannelTopic.value = false
  }
}

const togglingAnnouncementOnly = ref(false)
const announcementOnlyError = ref('')

async function onToggleAnnouncementOnly() {
  const s = session.session
  if (!s || !activeChannel.value) return
  announcementOnlyError.value = ''
  togglingAnnouncementOnly.value = true
  try {
    const updated = await s.updateChannel(guildId.value, activeChannel.value.id, {
      name: activeChannel.value.name,
      announcementOnly: !activeChannel.value.announcementOnly,
    })
    activeChannel.value = updated
    await refresh()
  } catch (e) {
    announcementOnlyError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    togglingAnnouncementOnly.value = false
  }
}

// Issue #458: non-member visibility for this channel — same idea as an
// event's own public toggle, newly available for channels since they had
// no non-member visibility concept before this ticket.
const togglingChannelPublic = ref(false)
const channelPublicError = ref('')

async function onToggleChannelPublic() {
  const s = session.session
  if (!s || !activeChannel.value) return
  channelPublicError.value = ''
  togglingChannelPublic.value = true
  try {
    const updated = await s.updateChannel(guildId.value, activeChannel.value.id, {
      name: activeChannel.value.name,
      public: !activeChannel.value.public,
    })
    activeChannel.value = updated
    await refresh()
  } catch (e) {
    channelPublicError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    togglingChannelPublic.value = false
  }
}

// --- Integrator affinity breakdown (issue #206, implementing decision #160) -----
// Aggregated from real IntegratorBinding (#83) data only — never a manager-added
// association (that's #20's superseded associate_integrator flow below). A
// manage_guild holder always sees it, gated server-side; anyone else only
// once the guild opts into public exposure via the toggle just below.
const integratorBreakdownLines = computed(() => {
  if (!integratorBreakdown.value) return []
  return integratorBreakdown.value.breakdown.map((entry) => formatGameBreakdownEntry(entry, integratorBreakdown.value!.totalMembers))
})
const integratorBreakdownEmpty = computed(
  () => integratorBreakdown.value !== null && hasNoGameBreakdownData(integratorBreakdown.value.breakdown),
)

const savingGameBreakdownPublic = ref(false)
const integratorBreakdownPublicError = ref('')

async function onToggleGameBreakdownPublic(next: boolean) {
  const s = session.session
  if (!s) return
  integratorBreakdownPublicError.value = ''
  savingGameBreakdownPublic.value = true
  try {
    await s.updateGuild(guildId.value, { gameBreakdownPublic: next })
    await refresh()
  } catch (e) {
    integratorBreakdownPublicError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingGameBreakdownPublic.value = false
  }
}

// --- Favorite integrators: curated top-5 pin list (issue #207, implementing -----
// decision #160). Always part of the public profile (`guild.favorite_games`
// — unlike the breakdown above, no public-exposure toggle of its own), so
// it's readable here whether or not the caller can manage the guild.
// Pinning is only ever offered from `integratorBreakdown.value.breakdown` — the
// same real-affinity data #206 already gates behind manage_guild — so a
// manager can never even attempt to pin an integrator without real affinity.
const favorites = computed(() => guild.value?.favoriteGames ?? [])
const pinnableIntegrators = computed(() =>
  integratorBreakdown.value ? pinnableBreakdownEntries(integratorBreakdown.value.breakdown, favorites.value) : [],
)
const canPinMore = computed(() => canPinMoreFavorites(favorites.value))

const savingFavorites = ref(false)
const favoritesError = ref('')

async function applyFavoriteGameIds(integratorIds: string[]) {
  const s = session.session
  if (!s) return
  favoritesError.value = ''
  savingFavorites.value = true
  try {
    await s.setFavoriteGames(guildId.value, integratorIds)
    await refresh()
  } catch (e) {
    favoritesError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingFavorites.value = false
  }
}

function onPinFavorite(integratorId: string) {
  return applyFavoriteGameIds(addFavoriteGameId(favorites.value, integratorId))
}

function onUnpinFavorite(integratorId: string) {
  return applyFavoriteGameIds(removeFavoriteGameId(favorites.value, integratorId))
}

function onReorderFavorite(integratorId: string, direction: 'up' | 'down') {
  return applyFavoriteGameIds(reorderFavoriteGameIds(favorites.value, integratorId, direction))
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

// "Members currently playing" (#57): realtime presence grouped by integrator,
// never a durable stat and never phrased as the guild belonging to an integrator
// (#74 — "N members playing X", not "Integrator X's guild"). Computed from the
// same roster/presence merge the roles view already loads, so it's always
// null in practice today (no integrator publishes presence.playing yet — see
// api/guilds.ts's own note) and renders the honest empty state below
// rather than fabricating activity.
const playingGroups = computed(() => groupMembersPlayingByIntegrator(members.value))

// --- Rename / retag / redescribe / MOTD / banner / icon --------------------

const savingField = ref<string | null>(null)
const fieldErrors = ref<Record<string, string>>({})

async function saveGuildField(
  field: 'name' | 'tag' | 'description' | 'motd' | 'banner' | 'icon',
  value: string,
) {
  const s = session.session
  if (!s) return
  fieldErrors.value[field] = ''
  savingField.value = field
  try {
    await s.updateGuild(guildId.value, { [field]: value })
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
  const s = session.session
  if (!s) return
  editGuildInfoError.value = ''
  savingGuildInfo.value = true
  try {
    await s.updateGuild(guildId.value, {
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
  const s = session.session
  if (!s) return
  recruitingError.value = ''
  savingRecruiting.value = true
  try {
    await s.updateGuild(guildId.value, { recruiting: next })
    await refresh()
  } catch (e) {
    recruitingError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingRecruiting.value = false
  }
}

// Issue #449: independent of recruiting — see Guild.public's own doc
// comment server-side.
const savingPublic = ref(false)
const publicError = ref('')

async function onTogglePublic(next: boolean) {
  const s = session.session
  if (!s) return
  publicError.value = ''
  savingPublic.value = true
  try {
    await s.updateGuild(guildId.value, { public: next })
    await refresh()
  } catch (e) {
    publicError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingPublic.value = false
  }
}

// Issue #87 — the guild's roster-visibility baseline. Recruiting/Public
// above (#449/#455) can still widen exposure beyond whatever this is set
// to; this only controls the underlying value.
const VISIBILITY_OPTIONS = [
  { value: 'public', label: 'Anyone' },
  { value: 'authenticated_only', label: 'Any signed-in user' },
  { value: 'friends', label: "Members' friends" },
  { value: 'guild_members', label: 'Guild members only' },
  { value: 'private', label: 'Owner only' },
]
const rosterVisibility = ref('guild_members')
const savingRosterVisibility = ref(false)
const rosterVisibilityError = ref('')

watch(
  () => guild.value?.rosterVisibility,
  (value) => {
    if (value) rosterVisibility.value = value
  },
  { immediate: true },
)

async function onSaveRosterVisibility() {
  const s = session.session
  if (!s) return
  rosterVisibilityError.value = ''
  savingRosterVisibility.value = true
  try {
    await s.updateGuild(guildId.value, {
      rosterVisibility: rosterVisibility.value,
    })
    await refresh()
  } catch (e) {
    rosterVisibilityError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingRosterVisibility.value = false
  }
}

const savingJoinPolicy = ref(false)
const joinPolicyError = ref('')

async function onToggleJoinPolicy(next: 'invite_only' | 'open') {
  const s = session.session
  if (!s) return
  joinPolicyError.value = ''
  savingJoinPolicy.value = true
  try {
    await s.updateGuild(guildId.value, { joinPolicy: next })
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
  const s = session.session
  if (!s) return
  linksError.value = ''
  savingLinks.value = true
  try {
    await s.updateGuild(guildId.value, { links })
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

// Issue #152's closed badge vocabulary, matching
// avalon_protocol::guilds::{RoleBadgeIcon,RoleBadgeColor}::ALL exactly.
const ROLE_BADGE_ICON_OPTIONS: RoleBadgeIconId[] = [
  'shield',
  'crown',
  'star',
  'sword',
  'wrench',
  'heart',
  'flag',
  'bolt',
]
const ROLE_BADGE_COLOR_OPTIONS: RoleBadgeColorId[] = [
  'gray',
  'red',
  'orange',
  'gold',
  'green',
  'blue',
  'purple',
]
// Presentational only — no server-side hex vocabulary to match, these
// just need to visually distinguish the 7 closed color ids.
const ROLE_BADGE_COLOR_HEX: Record<RoleBadgeColorId, string> = {
  gray: '#8b95a6',
  red: '#e5484d',
  orange: '#f0763a',
  gold: '#d4a72c',
  green: '#3cb179',
  blue: '#3b82f6',
  purple: '#8b5cf6',
}

const showAddRole = ref(false)
const newRoleName = ref('')
const newRolePermissions = ref<string[]>([])
const newRoleDescription = ref('')
const newRoleBadgeIcon = ref<RoleBadgeIconId>('shield')
const newRoleBadgeColor = ref<RoleBadgeColorId>('gray')
const addingRole = ref(false)
const addRoleError = ref('')

function cancelAddRole() {
  showAddRole.value = false
  newRoleName.value = ''
  newRolePermissions.value = []
  newRoleDescription.value = ''
  newRoleBadgeIcon.value = 'shield'
  newRoleBadgeColor.value = 'gray'
  addRoleError.value = ''
}

async function onAddRole() {
  const s = session.session
  if (!s) return
  addRoleError.value = ''
  addingRole.value = true
  try {
    await s.createRole(
      guildId.value,
      newRoleName.value.trim(),
      newRolePermissions.value,
      newRoleDescription.value.trim(),
      { icon: newRoleBadgeIcon.value, color: newRoleBadgeColor.value },
    )
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

async function onTogglePermission(role: Role, permission: string, event: Event) {
  const checkbox = event.target as HTMLInputElement
  const wasChecked = role.permissions.includes(permission)

  // The owner's permission list is structural, not editable (server-side:
  // update_role rejects any `permissions` change on name_index 0 — the
  // owner's authority comes from guilds.owner, not this row, so it always
  // holds every permission). Reject client-side too, with a clear reason,
  // rather than round-tripping to the server just to find out.
  if (role.nameIndex === 0) {
    checkbox.checked = wasChecked
    permissionMatrixError.value = "The owner role always has every permission and can't be changed."
    return
  }

  const s = session.session
  if (!s) return
  permissionMatrixError.value = ''
  const key = `${role.nameIndex}:${permission}`
  togglingPermissionFor.value = key
  const next = wasChecked
    ? role.permissions.filter((p) => p !== permission)
    : [...role.permissions, permission]
  try {
    await s.updateRole(guildId.value, role.nameIndex, undefined, next)
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
const roleDescriptionDraft = ref('')
const roleBadgeIconDraft = ref<RoleBadgeIconId>('shield')
const roleBadgeColorDraft = ref<RoleBadgeColorId>('gray')
const renamingRoleFor = ref<number | null>(null)

function unlockRole(role: Role) {
  unlockedRoleIndex.value = role.nameIndex
  roleNameDraft.value = role.name
  roleDescriptionDraft.value = role.description
  roleBadgeIconDraft.value = (role.badge.icon as RoleBadgeIconId | null) ?? 'shield'
  roleBadgeColorDraft.value = (role.badge.color as RoleBadgeColorId | null) ?? 'gray'
}

function lockRole() {
  unlockedRoleIndex.value = null
}

// Discards any unsaved drafts and re-locks the row. Permission checkbox
// changes have no "draft" to discard — each toggle already saved
// immediately on click — so this only ever affects name/description/badge.
function cancelRoleEdit() {
  unlockedRoleIndex.value = null
  roleNameDraft.value = ''
  roleDescriptionDraft.value = ''
}

// Saves name, description, and badge together — all three live in the
// same unlocked-row draft state, so one save covers whichever of them
// changed rather than a separate round trip per field.
async function onSaveRoleEdits(role: Role) {
  const s = session.session
  if (!s) return
  const name = roleNameDraft.value.trim()
  if (!name) return
  permissionMatrixError.value = ''
  renamingRoleFor.value = role.nameIndex
  try {
    await s.updateRole(guildId.value, role.nameIndex, name, undefined, roleDescriptionDraft.value.trim(), {
      icon: roleBadgeIconDraft.value,
      color: roleBadgeColorDraft.value,
    })
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
function isBaseRole(role: Role): boolean {
  return BASE_ROLE_INDEXES.includes(role.nameIndex)
}

const deletingRoleFor = ref<number | null>(null)

async function onDeleteRole(role: Role) {
  const s = session.session
  if (!s) return
  permissionMatrixError.value = ''
  deletingRoleFor.value = role.nameIndex
  try {
    await s.deleteRole(guildId.value, role.nameIndex)
    if (unlockedRoleIndex.value === role.nameIndex) unlockedRoleIndex.value = null
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
  const s = session.session
  if (!s || !changingRoleFor.value) return
  changeRoleError.value = ''
  changingRole.value = true
  try {
    // #697/#698: signature-required when the new role grants
    // manage_roles/manage_members (an escalation) — AccountSession.
    // updateMemberRole signs unconditionally whenever this device holds a
    // local key, same posture the old manual signing here had.
    await s.updateMemberRole(guildId.value, changingRoleFor.value, roleChangeValue.value)
    changingRoleFor.value = null
    await refresh()
  } catch (e) {
    changeRoleError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    changingRole.value = false
  }
}

// Issue #393: opens a member's read-only profile card.
function onViewProfile(identityId: string) {
  router.push({ name: 'user-profile', params: { id: identityId } })
}

async function onKick(identityId: string) {
  const s = session.session
  if (!s) return
  actionError.value = ''
  try {
    await s.removeMember(guildId.value, identityId)
    await refresh()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

// --- Join requests (issue #242) ------------------------------------------
// `joinRequests` (pending-only, per useGuildDetail's default listJoinRequests
// call) is manage_members-gated server-side — empty here for anyone who
// isn't a manager, same non-fatal-403 posture integratorBreakdown already has.

const decidingRequestId = ref<string | null>(null)
const joinRequestsError = ref('')

async function onApproveJoinRequest(requestId: string) {
  const s = session.session
  if (!s) return
  joinRequestsError.value = ''
  decidingRequestId.value = requestId
  try {
    await s.approveJoinRequest(guildId.value, requestId)
    await refresh()
  } catch (e) {
    joinRequestsError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    decidingRequestId.value = null
  }
}

async function onRejectJoinRequest(requestId: string) {
  const s = session.session
  if (!s) return
  joinRequestsError.value = ''
  decidingRequestId.value = requestId
  try {
    await s.rejectJoinRequest(guildId.value, requestId)
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
  const s = session.session
  if (!s) return
  myJoinRequestError.value = ''
  applyingToJoin.value = true
  try {
    await s.createJoinRequest(guildId.value)
    await refresh()
  } catch (e) {
    myJoinRequestError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    applyingToJoin.value = false
  }
}

async function onWithdrawJoinRequest() {
  const s = session.session
  if (!s || !myJoinRequest.value) return
  myJoinRequestError.value = ''
  withdrawingJoinRequest.value = true
  try {
    await s.withdrawJoinRequest(guildId.value, myJoinRequest.value.id)
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

// Issue #392: accepts either a raw identity id or a display_name handle
// (#128, #510), the same convenience Friends.vue's onAddFriend already
// offers — a handle (anything that isn't a UUID) is resolved to an
// identity id first, since createGuildInvite always targets an identity
// id on the wire.
async function onInvite() {
  const s = session.session
  if (!s) return
  inviteError.value = ''
  inviteSuccessId.value = ''
  inviting.value = true
  try {
    const input = inviteIdentityId.value.trim()
    const to = isIdentityId(input) ? input : await s.resolveHandle(input)
    const invite = await s.createGuildInvite(guildId.value, to)
    // No endpoint lists a user's own pending guild invites yet (a real
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
  const s = session.session
  if (!s) return
  actionError.value = ''
  try {
    await s.joinGuild(guildId.value)
    await refresh()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

async function onLeave() {
  const s = session.session
  if (!s) return
  actionError.value = ''
  try {
    await s.leaveGuild(guildId.value)
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
  const s = session.session
  if (!s) return
  transferError.value = ''
  transferring.value = true
  try {
    const to = transferTo.value.trim()
    await s.transferOwnership(guildId.value, to)
    cancelTransfer()
    await refresh()
  } catch (e) {
    transferError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    transferring.value = false
  }
}

// --- Associate integrator -------------------------------------------------------
// No integrator registry/picker exists yet (#18's own deferral) — a plain
// integrator-id text input is milestone-1 scope, matching how "Add friend" took
// a raw identity id with no search.

const showAssociateIntegrator = ref(false)
const associateIntegratorId = ref('')
const associatingIntegrator = ref(false)
const associateIntegratorError = ref('')

function cancelAssociateIntegrator() {
  showAssociateIntegrator.value = false
  associateIntegratorId.value = ''
  associateIntegratorError.value = ''
}

async function onAssociateIntegrator() {
  const s = session.session
  if (!s) return
  associateIntegratorError.value = ''
  associatingIntegrator.value = true
  try {
    await s.associateIntegrator(guildId.value, associateIntegratorId.value.trim())
    cancelAssociateIntegrator()
    await refresh()
  } catch (e) {
    associateIntegratorError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    associatingIntegrator.value = false
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
  const s = session.session
  if (!s) return
  createChannelError.value = ''
  creatingChannel.value = true
  try {
    await s.createChannel(guildId.value, newChannelName.value.trim())
    cancelCreateChannel()
    await refresh()
  } catch (e) {
    createChannelError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    creatingChannel.value = false
  }
}

async function onArchiveChannel(channelId: string) {
  const s = session.session
  if (!s) return
  actionError.value = ''
  try {
    await s.archiveChannel(guildId.value, channelId)
    await refresh()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

// --- Events (issue #169) -----------------------------------------------

const sortedEvents = computed(() => sortByStartsAt(events.value))

// AvalonEventCard's own prop type (packages/ui) still uses the wire's
// snake_case not_going — independent of bindings/ts's own RsvpCounts.
function toRsvpCountsProp(counts: GuildEvent['rsvpCounts']) {
  return { going: counts.going, maybe: counts.maybe, not_going: counts.notGoing }
}

// --- Calendar tab: a navigable month view of the same events list above,
// grouped by local calendar day (localDateKey — see its own doc comment
// on why "local," not the raw UTC starts_at). No separate fetch: the
// guild's full event list is already loaded for the Events tab.
const today = new Date()
const calendarYear = ref(today.getFullYear())
const calendarMonth = ref(today.getMonth() + 1)
const calendarSelectedDate = ref<string | null>(null)

const calendarEventDates = computed(() => events.value.map((e) => localDateKey(e.startsAt)))

const calendarSelectedEvents = computed(() => {
  if (!calendarSelectedDate.value) return []
  return sortedEvents.value.filter((e) => localDateKey(e.startsAt) === calendarSelectedDate.value)
})

function onSelectCalendarDate(date: string) {
  calendarSelectedDate.value = calendarSelectedDate.value === date ? null : date
}

const showCreateEvent = ref(false)
const newEventTitle = ref('')
const newEventDescription = ref('')
const newEventStartsAt = ref('')
const newEventEndsAt = ref('')
// Issue #448: defaults to false — member-only, same as every event before
// this field existed. A guild opts an event into public visibility, not
// the other way around.
const newEventPublic = ref(false)
const creatingEvent = ref(false)
const createEventError = ref('')
// Issue #463. Non-null while the create-event form doubles as the
// edit-event form for this event id — same form, same fields, a
// different submit target (updateEvent instead of createEvent).
const editingEventId = ref<string | null>(null)

function cancelCreateEvent() {
  showCreateEvent.value = false
  editingEventId.value = null
  newEventTitle.value = ''
  newEventDescription.value = ''
  newEventStartsAt.value = ''
  newEventEndsAt.value = ''
  newEventPublic.value = false
  createEventError.value = ''
}

function onEditEvent(event: GuildEvent) {
  editingEventId.value = event.id
  newEventTitle.value = event.title
  newEventDescription.value = event.description ?? ''
  newEventStartsAt.value = toLocalDateTimeInput(event.startsAt)
  newEventEndsAt.value = event.endsAt ? toLocalDateTimeInput(event.endsAt) : ''
  newEventPublic.value = event.public
  createEventError.value = ''
  showCreateEvent.value = true
}

async function onCreateEvent() {
  const s = session.session
  if (!s) return
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
    const fields = {
      title: newEventTitle.value.trim(),
      description: newEventDescription.value.trim() || undefined,
      startsAt: startsAtIso,
      endsAt: endsAtIso || undefined,
      public: newEventPublic.value,
    }
    if (editingEventId.value) {
      await s.updateEvent(guildId.value, editingEventId.value, fields)
    } else {
      await s.createEvent(guildId.value, fields)
    }
    cancelCreateEvent()
    await refresh()
  } catch (e) {
    createEventError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    creatingEvent.value = false
  }
}

const deletingEventId = ref<string | null>(null)

async function onDeleteEvent(event: GuildEvent) {
  const s = session.session
  if (!s) return
  if (!window.confirm(`Delete "${event.title}"? This can't be undone.`)) return
  actionError.value = ''
  deletingEventId.value = event.id
  try {
    await s.deleteEvent(guildId.value, event.id)
    await refresh()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    deletingEventId.value = null
  }
}

// Self-service only, always the caller's own RSVP — see
// AvalonRsvpControl.types.ts and guild_events.rs::upsert_rsvp.
async function onRsvp(eventId: string, status: 'going' | 'maybe' | 'not_going') {
  const s = session.session
  if (!s) return
  actionError.value = ''
  try {
    await s.rsvpToEvent(guildId.value, eventId, status)
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
          {{ guild.memberCount }} member{{ guild.memberCount === 1 ? '' : 's' }} ·
          {{ guild.joinPolicy === 'open' ? 'Open to join' : 'Invite only' }}
        </p>
      </div>
    </header>

    <!-- Issue #276: MOTD moved here from the Overview tab's About card so
         it's visible near the top of the guild page regardless of which
         tab is active — an MOTD nobody navigates to see isn't functioning
         as one. -->
    <div v-if="guild.motd" :class="local.motdBanner">
      <span :class="local.motdBannerLabel">MOTD</span>
      <span>{{ guild.motd }}</span>
    </div>

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
        v-for="tab in visibleTabs"
        :key="tab.key"
        type="button"
        :class="[local.tab, activeTab === tab.key && local.tabActive]"
        @click="selectTab(tab.key)"
      >
        {{ tab.label }}
      </button>
    </div>

    <!-- Overview: header info already above (MOTD is now its own banner
         above the tab bar, issue #276), plus links, integrator affinity, favorite
         integrators, associated integrators, and guild history — all read-only here;
         editing lives in Settings. -->
    <div v-if="activeTab === 'overview'" :class="styles.grid">
      <div :class="styles.mainColumn">
        <AvalonCard v-if="guildLinks.length > 0" title="About">
          <p v-for="link in guildLinks" :key="link.url" :class="styles.empty">
            <a :href="link.url" target="_blank" rel="noopener noreferrer">{{ link.label }}</a>
          </p>
        </AvalonCard>

        <AvalonCard
          v-if="canManageGuild || integratorBreakdown"
          title="Integrator affinity"
          subtitle="Auto-derived from members' active integrator bindings — not something anyone sets by hand. Managers can choose whether it's visible on this guild's public profile and discovery card; it's always visible to members."
        >
          <p v-if="canManageGuild" :class="styles.empty">
            Shown on this guild's public profile and discovery card:
            {{ guild.gameBreakdownPublic ? 'yes' : 'no' }}
          </p>
          <AvalonButton
            v-if="canManageGuild"
            :label="
              savingGameBreakdownPublic
                ? 'Saving…'
                : guild.gameBreakdownPublic
                  ? 'Hide from public profile'
                  : 'Show on public profile'
            "
            variant="secondary"
            @click="onToggleGameBreakdownPublic(!guild.gameBreakdownPublic)"
          />
          <p v-if="integratorBreakdownPublicError" :class="styles.error">{{ integratorBreakdownPublicError }}</p>

          <p v-if="integratorBreakdownError && !canManageGuild" :class="styles.empty">
            This guild hasn't shared its integrator affinity breakdown publicly.
          </p>
          <template v-else>
            <p v-for="line in integratorBreakdownLines" :key="line" :class="styles.empty">{{ line }}</p>
            <p v-if="integratorBreakdownEmpty" :class="styles.empty">
              No guild member has an active integrator binding yet.
            </p>
          </template>
        </AvalonCard>

        <AvalonCard v-if="canManageGuild || favorites.length > 0" title="Favorite integrators">
          <p v-if="favorites.length === 0" :class="styles.empty">No favorite integrators pinned yet.</p>
          <div v-for="(entry, index) in favorites" :key="entry.integratorId" :class="styles.empty">
            {{ formatFavoriteGameEntry(entry) }}
            <template v-if="canManageGuild">
              <AvalonButton
                v-if="index > 0"
                label="Move up"
                variant="secondary"
                :disabled="savingFavorites"
                @click="onReorderFavorite(entry.integratorId, 'up')"
              />
              <AvalonButton
                v-if="index < favorites.length - 1"
                label="Move down"
                variant="secondary"
                :disabled="savingFavorites"
                @click="onReorderFavorite(entry.integratorId, 'down')"
              />
              <AvalonButton
                label="Unpin"
                variant="danger"
                :disabled="savingFavorites"
                @click="onUnpinFavorite(entry.integratorId)"
              />
            </template>
          </div>

          <template v-if="canManageGuild">
            <p v-if="!canPinMore" :class="styles.empty">Up to 5 integrators may be pinned at once.</p>
            <p v-else-if="pinnableIntegrators.length === 0" :class="styles.empty">
              No unpinned integrator currently has affinity to pin.
            </p>
            <div v-for="entry in pinnableIntegrators" :key="entry.integratorId" :class="styles.empty">
              {{ entry.integratorName }}
              <AvalonButton
                label="Pin"
                variant="secondary"
                :disabled="savingFavorites"
                @click="onPinFavorite(entry.integratorId)"
              />
            </div>
            <p v-if="favoritesError" :class="styles.error">{{ favoritesError }}</p>
          </template>
        </AvalonCard>

        <!--
          Associated integrators (#20's original manual associate_integrator flow,
          superseded by #206/#207's real-binding-derived affinity above but
          still live): now visible to any member, read-only — only the
          "associate an integrator" action itself stays canManageGuild-gated
          (issue #241).
        -->
        <AvalonCard v-if="canManageGuild || guild.integrators.length > 0" title="Associated integrators">
          <p v-for="integratorId in guild.integrators" :key="integratorId" :class="styles.empty">{{ integratorId }}</p>
          <p v-if="guild.integrators.length === 0" :class="styles.empty">No integrators associated yet.</p>
          <template v-if="canManageGuild">
            <AvalonButton
              v-show="!showAssociateIntegrator"
              label="Associate an integrator"
              variant="secondary"
              @click="showAssociateIntegrator = true"
            />
            <div v-show="showAssociateIntegrator">
              <AvalonForm
                submit-label="Associate"
                :submitting="associatingIntegrator"
                :error="associateIntegratorError"
                @submit="onAssociateIntegrator"
              >
                <AvalonTextField v-model="associateIntegratorId" label="Integrator id" />
                <template #secondary-actions>
                  <AvalonButton label="Cancel" variant="secondary" @click="cancelAssociateIntegrator" />
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
            v-if="guild.joinPolicy === 'open' && !isMember"
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
          <template v-if="guild.joinPolicy !== 'open' && !isMember">
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
              @view="onViewProfile(member.identityId)"
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
                  <option v-for="role in roles" :key="role.nameIndex" :value="role.nameIndex">
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
            <p v-for="group in playingGroups" :key="group.integratorId" :class="styles.empty">
              {{ formatPlayingSummary(group) }}
            </p>
          </template>
          <p :class="styles.empty">Live presence, not a durable stat — updates as members' status changes.</p>
        </AvalonCard>

        <AvalonCard v-if="canManageMembers" title="Invite a user">
          <p v-if="inviteSuccessId" :class="styles.empty">
            Invite sent — they'll see it on their Guilds page.
          </p>
          <AvalonButton
            v-show="!showInvite"
            label="Invite a user"
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
              <AvalonTextField
                v-model="inviteIdentityId"
                label="Identity id or handle"
                placeholder="Identity id, or display name"
              />
              <template #secondary-actions>
                <AvalonButton label="Cancel" variant="secondary" @click="cancelInvite" />
              </template>
            </AvalonForm>
          </div>
        </AvalonCard>

        <!--
          Issue #242: applicant-initiated join requests, the counterpart to
          "Invite a user" above. manage_members-gated same as that card
          (the server independently enforces this — joinRequests is simply
          empty for anyone else). Pending only, matching
          crates/server/src/guilds.rs::list_join_requests' own default.
        -->
        <AvalonCard v-if="canManageMembers" title="Applications">
          <p v-if="joinRequests.length === 0" :class="styles.empty">No pending applications.</p>
          <div v-for="request in joinRequests" :key="request.id" :class="local.applicationRow">
            <button
              type="button"
              :class="local.applicantName"
              @click="onViewProfile(request.applicant)"
            >
              {{ applicantNames[request.applicant] ?? request.applicant }}
            </button>
            <span v-if="request.message" :class="styles.empty">— "{{ request.message }}"</span>
            <span :class="local.applicationActions">
              <button
                type="button"
                :class="local.approveButton"
                aria-label="Approve"
                :disabled="decidingRequestId === request.id"
                @click="onApproveJoinRequest(request.id)"
              >
                <AvalonIcon name="check" :size="16" />
              </button>
              <button
                type="button"
                :class="local.rejectButton"
                aria-label="Reject"
                :disabled="decidingRequestId === request.id"
                @click="onRejectJoinRequest(request.id)"
              >
                <AvalonIcon name="close" :size="16" />
              </button>
            </span>
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

            <!-- Channel topic (issue #276): read-only for anyone who can't
                 manage channels, inline-editable (AvalonEditableField, same
                 component Settings uses for the guild's own MOTD/banner/icon)
                 for whoever can. -->
            <AvalonEditableField
              v-if="activeChannel && canManageChannels"
              label="Topic"
              :value="activeChannel.topic ?? ''"
              empty-text="No topic set"
              placeholder="What's this channel for?"
              :saving="savingChannelTopic"
              :error="channelTopicError"
              @save="onSaveChannelTopic"
            />
            <p v-else-if="activeChannel?.topic" :class="local.channelTopic">{{ activeChannel.topic }}</p>

            <!-- Announcement-only (issue #250, relocated by #276 out of
                 ResourcePermissionOverrides.vue's role x permission grid —
                 a channel manager expects to find this next to the
                 channel's own settings, not buried in a permissions
                 matrix). -->
            <label v-if="activeChannel && canManageChannels" :class="local.announcementRow">
              <input
                type="checkbox"
                :checked="activeChannel.announcementOnly"
                :disabled="togglingAnnouncementOnly"
                @change="onToggleAnnouncementOnly"
              />
              <span
                >Announcement-only — only roles allowed <code>channel_post</code> here (or granted
                it below) may post</span
              >
            </label>
            <p v-if="announcementOnlyError" :class="styles.error">{{ announcementOnlyError }}</p>

            <!-- Non-member visibility (issue #458) — only matters while
                 this guild is Public (Settings), same framing the event
                 public toggle already uses. -->
            <label v-if="activeChannel && canManageChannels" :class="local.announcementRow">
              <input
                type="checkbox"
                :checked="activeChannel.public"
                :disabled="togglingChannelPublic"
                @change="onToggleChannelPublic"
              />
              <span>Visible to prospective members (only matters while this guild is Public)</span>
            </label>
            <p v-if="channelPublicError" :class="styles.error">{{ channelPublicError }}</p>

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
                :sent-at-label="new Date(message.sentAt).toLocaleString()"
                :can-delete="canDeleteMessage"
                :is-own="message.author === selfId"
                :presence-status="memberPresence[message.author]"
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

          <!-- Per-resource permission overrides (issue #250, generalized
               by #458 to also cover view/view_details): per-role
               overrides for the active channel — the announcement-only
               toggle moved above (issue #276) so this stays focused
               purely on the role x permission grid. Visible to anyone who
               can manage roles — server re-checks each action
               independently. -->
          <ResourcePermissionOverrides
            v-if="activeChannel && canManageRoles && session.session"
            :session="session.session"
            :guild-id="guildId"
            resource-kind="channel"
            :resource-id="activeChannel.id"
            :roles="roles"
            :can-manage-roles="canManageRoles"
          />
        </template>
      </div>
    </div>

    <!--
      Guild events calendar + RSVP (issue #169). Neither an event nor
      an RSVP row is durable protocol history — see
      docs/architecture/guilds.md's "Guild events calendar + RSVP"
      section — so nothing here claims to be permanent. `EventResponse`
      includes the caller's own RSVP status (issue #463), so
      AvalonRsvpControl pre-selects it correctly.
    -->
    <div v-else-if="activeTab === 'events'" :class="styles.mainColumn">
        <AvalonCard title="Events">
          <p v-if="!isMember" :class="styles.empty">
            Showing this guild's public events. Join to see everything and RSVP.
          </p>
          <p v-if="sortedEvents.length === 0" :class="styles.empty">No upcoming events yet.</p>
          <div
            v-for="event in sortedEvents"
            :key="event.id"
            :class="isMember && event.detailsVisible ? local.eventCardClickable : undefined"
            @click="isMember && event.detailsVisible && openRsvpRoster(event.id, event.title)"
          >
            <AvalonEventCard
              :title="event.title"
              :description="event.description ?? undefined"
              :starts-at="event.startsAt"
              :ends-at="event.endsAt ?? undefined"
              :rsvp-counts="toRsvpCountsProp(event.rsvpCounts)"
              :details-visible="event.detailsVisible"
            >
              <template #actions>
                <div v-if="isMember && event.detailsVisible" @click.stop>
                  <AvalonRsvpControl :current-status="event.myRsvp ?? undefined" @rsvp="(status) => onRsvp(event.id, status)" />
                </div>
                <div v-if="canManageEvents" @click.stop :class="local.eventManageActions">
                  <AvalonButton label="Edit" variant="secondary" @click="onEditEvent(event)" />
                  <AvalonButton
                    :label="deletingEventId === event.id ? 'Deleting…' : 'Delete'"
                    variant="danger"
                    :disabled="deletingEventId === event.id"
                    @click="onDeleteEvent(event)"
                  />
                </div>
              </template>
            </AvalonEventCard>
          </div>
          <AvalonButton
            v-if="canManageEvents && !showCreateEvent"
            label="+ New event"
            variant="secondary"
            @click="showCreateEvent = true"
          />
          <div v-if="showCreateEvent">
            <AvalonForm
              :submit-label="editingEventId ? 'Save changes' : 'Create event'"
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
              <label :class="local.publicEventToggle">
                <input v-model="newEventPublic" type="checkbox" />
                Visible to prospective members (only matters while this guild is Public — see Settings)
              </label>
              <template #secondary-actions>
                <AvalonButton label="Cancel" variant="secondary" @click="cancelCreateEvent" />
              </template>
            </AvalonForm>

            <!-- Per-resource permission overrides (issue #458) — only
                 while editing an existing event (there's no resource id
                 yet while creating one). Same role x permission grid the
                 Channels tab uses. -->
            <ResourcePermissionOverrides
              v-if="editingEventId && canManageRoles && session.session"
              :session="session.session"
              :guild-id="guildId"
              resource-kind="event"
              :resource-id="editingEventId"
              :roles="roles"
              :can-manage-roles="canManageRoles"
            />
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
              :class="isMember && event.detailsVisible ? local.eventCardClickable : undefined"
              @click="isMember && event.detailsVisible && openRsvpRoster(event.id, event.title)"
            >
              <AvalonEventCard
                :title="event.title"
                :description="event.description ?? undefined"
                :starts-at="event.startsAt"
                :ends-at="event.endsAt ?? undefined"
                :rsvp-counts="toRsvpCountsProp(event.rsvpCounts)"
                :details-visible="event.detailsVisible"
              >
                <template v-if="isMember && event.detailsVisible" #actions>
                  <div @click.stop>
                    <AvalonRsvpControl :current-status="event.myRsvp ?? undefined" @rsvp="(status) => onRsvp(event.id, status)" />
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
                <tr v-for="role in roles" :key="role.nameIndex">
                  <td>
                    <div :class="local.roleNameCell">
                      <button
                        type="button"
                        :class="local.roleLockButton"
                        :aria-label="unlockedRoleIndex === role.nameIndex ? 'Lock role' : 'Unlock role to edit'"
                        @click="
                          unlockedRoleIndex === role.nameIndex
                            ? lockRole()
                            : unlockRole(role)
                        "
                      >
                        <AvalonIcon :name="unlockedRoleIndex === role.nameIndex ? 'check' : 'pencil'" :size="14" />
                      </button>
                      <button
                        v-if="unlockedRoleIndex === role.nameIndex"
                        type="button"
                        :class="local.roleLockButton"
                        aria-label="Cancel editing"
                        @click="cancelRoleEdit"
                      >
                        <AvalonIcon name="close" :size="14" />
                      </button>
                      <span :style="{ color: ROLE_BADGE_COLOR_HEX[(role.badge.color as RoleBadgeColorId | null) ?? 'gray'] }">
                        <AvalonIcon :name="(role.badge.icon ?? 'shield') as AvalonIconName" :size="14" />
                      </span>
                      <input
                        v-if="unlockedRoleIndex === role.nameIndex"
                        v-model="roleNameDraft"
                        :class="local.roleNameInput"
                        type="text"
                        :disabled="renamingRoleFor === role.nameIndex"
                      />
                      <span v-else>{{ role.name }}</span>
                    </div>
                    <div v-if="unlockedRoleIndex === role.nameIndex" :class="local.roleNameCell">
                      <input
                        v-model="roleDescriptionDraft"
                        :class="local.roleNameInput"
                        type="text"
                        placeholder="Description"
                        :disabled="renamingRoleFor === role.nameIndex"
                      />
                      <select v-model="roleBadgeIconDraft" aria-label="Badge icon">
                        <option v-for="icon in ROLE_BADGE_ICON_OPTIONS" :key="icon" :value="icon">
                          {{ icon }}
                        </option>
                      </select>
                      <select v-model="roleBadgeColorDraft" aria-label="Badge color">
                        <option v-for="color in ROLE_BADGE_COLOR_OPTIONS" :key="color" :value="color">
                          {{ color }}
                        </option>
                      </select>
                      <AvalonButton
                        :label="renamingRoleFor === role.nameIndex ? 'Saving…' : 'Save'"
                        variant="secondary"
                        :disabled="renamingRoleFor === role.nameIndex"
                        @click="onSaveRoleEdits(role)"
                      />
                    </div>
                    <p v-else-if="role.description" :class="styles.empty">{{ role.description }}</p>
                  </td>
                  <td v-for="permission in PERMISSION_OPTIONS" :key="permission">
                    <input
                      type="checkbox"
                      :checked="role.permissions.includes(permission)"
                      :disabled="
                        unlockedRoleIndex !== role.nameIndex ||
                        togglingPermissionFor === `${role.nameIndex}:${permission}`
                      "
                      @change="onTogglePermission(role, permission, $event)"
                    />
                  </td>
                  <td>
                    <AvalonButton
                      v-if="unlockedRoleIndex === role.nameIndex && !isBaseRole(role)"
                      :label="deletingRoleFor === role.nameIndex ? 'Deleting…' : 'Delete'"
                      variant="danger"
                      :disabled="deletingRoleFor === role.nameIndex"
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
              <AvalonTextField
                v-model="newRoleDescription"
                label="Description"
                placeholder="Leads scheduled raids"
              />
              <label>
                Badge icon
                <select v-model="newRoleBadgeIcon" aria-label="Badge icon">
                  <option v-for="icon in ROLE_BADGE_ICON_OPTIONS" :key="icon" :value="icon">
                    {{ icon }}
                  </option>
                </select>
              </label>
              <label>
                Badge color
                <select v-model="newRoleBadgeColor" aria-label="Badge color">
                  <option v-for="color in ROLE_BADGE_COLOR_OPTIONS" :key="color" :value="color">
                    {{ color }}
                  </option>
                </select>
              </label>
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
        <AvalonCard v-else title="Roles" subtitle="Read-only — you don't have permission to manage this guild's roles.">
          <div :class="local.permissionMatrixScroll">
            <table :class="local.permissionMatrix">
              <thead>
                <tr>
                  <th>Role</th>
                  <th v-for="permission in PERMISSION_OPTIONS" :key="permission">{{ permission }}</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="role in roles" :key="role.nameIndex">
                  <td>
                    <span :style="{ color: ROLE_BADGE_COLOR_HEX[(role.badge.color as RoleBadgeColorId | null) ?? 'gray'] }">
                      <AvalonIcon :name="(role.badge.icon ?? 'shield') as AvalonIconName" :size="14" />
                    </span>
                    {{ role.name }}
                    <p v-if="role.description" :class="styles.empty">{{ role.description }}</p>
                  </td>
                  <td v-for="permission in PERMISSION_OPTIONS" :key="permission">
                    <AvalonIcon
                      v-if="role.permissions.includes(permission)"
                      name="check"
                      :size="14"
                    />
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </AvalonCard>
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
              Recruiting guilds are discoverable on the "Discover" board and accept join requests:
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
            title="Public"
            subtitle="Separate from recruiting above: this makes your roster (and, once event visibility ships, your public events) visible to anyone signed in, whether or not you're actively recruiting."
          >
            <p :class="styles.empty">
              Anyone signed in can see this guild's roster: {{ guild.public ? 'yes' : 'no' }}
            </p>
            <AvalonButton
              :label="savingPublic ? 'Saving…' : guild.public ? 'Make private' : 'Make public'"
              variant="secondary"
              @click="onTogglePublic(!guild.public)"
            />
            <p v-if="publicError" :class="styles.error">{{ publicError }}</p>
          </AvalonCard>

          <AvalonCard
            title="Roster visibility"
            subtitle="The baseline for who can see this guild's member list — Recruiting and Public above can still widen it further."
          >
            <select v-model="rosterVisibility">
              <option v-for="opt in VISIBILITY_OPTIONS" :key="opt.value" :value="opt.value">
                {{ opt.label }}
              </option>
            </select>
            <AvalonButton
              :label="savingRosterVisibility ? 'Saving…' : 'Save'"
              variant="secondary"
              :disabled="savingRosterVisibility"
              @click="onSaveRosterVisibility"
            />
            <p v-if="rosterVisibilityError" :class="styles.error">{{ rosterVisibilityError }}</p>
          </AvalonCard>

          <AvalonCard
            title="Membership"
            subtitle="Separate from recruiting above: this controls whether joining requires an invite or approval at all."
          >
            <p :class="styles.empty">
              {{
                guild.joinPolicy === 'open'
                  ? 'Open — anyone can join instantly, no invite or approval needed.'
                  : 'Invite only — joining requires an invite, or an application a manager approves.'
              }}
            </p>
            <AvalonButton
              :label="
                savingJoinPolicy
                  ? 'Saving…'
                  : guild.joinPolicy === 'open'
                    ? 'Switch to invite only'
                    : 'Switch to open'
              "
              variant="secondary"
              @click="onToggleJoinPolicy(guild.joinPolicy === 'open' ? 'invite_only' : 'open')"
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
