<script setup lang="ts">
// Guild overview / roster / roles / channels (issue #24). Management
// controls are shown only when the caller's own role grants the matching
// permission (crates/server/src/guilds.rs's fixed permission set) — the
// server re-checks independently and is the real authority, so a 403 here
// is possible and shown as a plain error rather than crashing the page.
import { computed, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import {
  AvalonButton,
  AvalonCard,
  AvalonChannelList,
  AvalonEditableField,
  AvalonEventCard,
  AvalonFilterBar,
  AvalonForm,
  AvalonGuildMemberRow,
  AvalonRsvpControl,
  AvalonTextField,
} from '@avalon/ui'
import * as api from '../api/client'
import { sortByStartsAt, validateEventForm } from '../api/guildEvents'
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
import { useGuildDetail } from '../composables/useGuildDetail'
import { useSessionStore } from '../stores/session'
import styles from './page.module.scss'

const PERMISSION_OPTIONS = ['manage_guild', 'manage_roles', 'manage_members', 'manage_channels']

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

// --- Rename / retag / redescribe ------------------------------------------

const savingField = ref<string | null>(null)
const fieldErrors = ref<Record<string, string>>({})

async function saveGuildField(field: 'name' | 'tag' | 'description', value: string) {
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

// --- Channels ---------------------------------------------------------

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

function onSelectChannel(channelId: string) {
  router.push({ name: 'guild-channel', params: { id: guildId.value, cid: channelId } })
}

// --- Events (issue #169) -----------------------------------------------

const sortedEvents = computed(() => sortByStartsAt(events.value))

const showCreateEvent = ref(false)
const newEventTitle = ref('')
const newEventDescription = ref('')
const newEventStartsAt = ref('')
const creatingEvent = ref(false)
const createEventError = ref('')

function cancelCreateEvent() {
  showCreateEvent.value = false
  newEventTitle.value = ''
  newEventDescription.value = ''
  newEventStartsAt.value = ''
  createEventError.value = ''
}

async function onCreateEvent() {
  if (!session.token) return
  createEventError.value = ''
  const startsAtIso = newEventStartsAt.value ? new Date(newEventStartsAt.value).toISOString() : ''
  const validation = validateEventForm({
    title: newEventTitle.value,
    description: newEventDescription.value,
    startsAt: startsAtIso,
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
</script>

<template>
  <div v-if="!loading && guild" :class="styles.page">
    <header :class="styles.pageHeader">
      <template v-if="canManageGuild">
        <AvalonEditableField
          label="Name"
          :value="guild.name"
          :saving="savingField === 'name'"
          :error="fieldErrors.name"
          @save="saveGuildField('name', $event)"
        />
        <AvalonEditableField
          label="Tag"
          :value="guild.tag"
          :saving="savingField === 'tag'"
          :error="fieldErrors.tag"
          @save="saveGuildField('tag', $event)"
        />
        <AvalonEditableField
          label="Description"
          :value="guild.description"
          empty-text="No description"
          :saving="savingField === 'description'"
          :error="fieldErrors.description"
          @save="saveGuildField('description', $event)"
        />
      </template>
      <template v-else>
        <h1 :class="styles.title">{{ guild.name }} [{{ guild.tag }}]</h1>
        <p v-if="guild.description" :class="styles.subtitle">{{ guild.description }}</p>
      </template>
      <p :class="styles.subtitle">
        {{ guild.member_count }} member{{ guild.member_count === 1 ? '' : 's' }} ·
        {{ guild.join_policy === 'open' ? 'Open to join' : 'Invite only' }}
      </p>
    </header>

    <p v-if="error" :class="styles.error">{{ error }}</p>
    <p v-if="actionError" :class="styles.error">{{ actionError }}</p>

    <div :class="styles.grid">
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

        <AvalonCard v-if="canManageRoles" title="Roles">
          <p v-for="role in roles" :key="role.name_index" :class="styles.empty">
            {{ role.name }} — {{ role.permissions.join(', ') || 'no permissions' }}
          </p>
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

        <AvalonCard title="Channels">
          <AvalonChannelList
            :channels="channels"
            :can-manage="canManageChannels"
            @select="onSelectChannel"
            @create="showCreateChannel = true"
            @archive="onArchiveChannel"
          />
          <p :class="styles.empty">
            Message history is subject to the server's retention policy, not permanent.
          </p>
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
        <AvalonCard title="Events">
          <p v-if="sortedEvents.length === 0" :class="styles.empty">No upcoming events yet.</p>
          <AvalonEventCard
            v-for="event in sortedEvents"
            :key="event.id"
            :title="event.title"
            :description="event.description ?? undefined"
            :starts-at="event.starts_at"
            :ends-at="event.ends_at ?? undefined"
            :rsvp-counts="event.rsvp_counts"
          >
            <template #actions>
              <AvalonRsvpControl @rsvp="(status) => onRsvp(event.id, status)" />
            </template>
          </AvalonEventCard>
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
              <AvalonTextField
                v-model="newEventStartsAt"
                label="Starts at"
                placeholder="2026-09-15T20:00"
              />
              <template #secondary-actions>
                <AvalonButton label="Cancel" variant="secondary" @click="cancelCreateEvent" />
              </template>
            </AvalonForm>
          </div>
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

        <AvalonCard title="Membership">
          <p :class="styles.empty">{{ membershipStatus }}</p>
          <AvalonButton
            v-if="guild.join_policy === 'open' && !isMember"
            label="Join guild"
            variant="primary"
            @click="onJoin"
          />
          <AvalonButton v-if="isMember && !isOwner" label="Leave guild" variant="danger" @click="onLeave" />
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
          Issue #206 (implementing decision #160): a read-only, derived
          breakdown of which games guildmates actually play, aggregated
          from real GameBinding (#83) data — never a manually-declared
          association. Shown whenever there's something to show: a
          manage_guild holder sees it (and the public-exposure toggle)
          regardless of the toggle's own state; anyone else only once
          `gameBreakdown` successfully loads, which the server itself
          gates on `guild.game_breakdown_public`.
        -->
        <AvalonCard v-if="canManageGuild || gameBreakdown" title="Game affinity">
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

        <!--
          Issue #207 (implementing decision #160): a manage_guild-curated
          top-5 subset of the affinity breakdown above, always shown on the
          public profile (guild.favorite_games) — visible to anyone once
          there's something to show, with pin/unpin/reorder controls added
          for a manage_guild holder. A pin can only ever be added from
          `pinnableGames` (games `gameBreakdown` already shows real
          affinity for), so there's no path to pinning an unaffiliated game
          from this UI.
        -->
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

        <AvalonCard v-if="canManageGuild" title="Associated games">
          <p v-for="gameId in guild.games" :key="gameId" :class="styles.empty">{{ gameId }}</p>
          <p v-if="guild.games.length === 0" :class="styles.empty">No games associated yet.</p>
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
        </AvalonCard>

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
    </div>
  </div>
</template>
