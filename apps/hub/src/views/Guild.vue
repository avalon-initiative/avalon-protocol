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
  AvalonForm,
  AvalonGuildMemberRow,
  AvalonTextField,
} from '@avalon/ui'
import * as api from '../api/client'
import {
  canChangeMemberRole,
  canKickMember,
  groupMembersByRole,
  hasGuildPermission,
  roleVariantForIndex,
} from '../api/guilds'
import { useGuildDetail } from '../composables/useGuildDetail'
import { useSessionStore } from '../stores/session'
import styles from './page.module.scss'

const PERMISSION_OPTIONS = ['manage_guild', 'manage_roles', 'manage_members', 'manage_channels']

const route = useRoute()
const router = useRouter()
const session = useSessionStore()

const guildId = computed(() => route.params.id as string)
const { guild, roles, members, channels, selfId, selfPermissions, isOwner, loading, error, refresh } =
  useGuildDetail(guildId)

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
const roleGroups = computed(() => groupMembersByRole(members.value, roles.value))

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
            </AvalonForm>
            <AvalonButton label="Cancel" variant="secondary" @click="cancelChangeRole" />
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
            </AvalonForm>
            <AvalonButton label="Cancel" variant="secondary" @click="cancelAddRole" />
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
            </AvalonForm>
            <AvalonButton label="Cancel" variant="secondary" @click="cancelCreateChannel" />
          </div>
        </AvalonCard>
      </div>

      <div :class="styles.sideColumn">
        <AvalonCard title="Membership">
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
            </AvalonForm>
            <AvalonButton label="Cancel" variant="secondary" @click="cancelInvite" />
          </div>
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
            </AvalonForm>
            <AvalonButton label="Cancel" variant="secondary" @click="cancelAssociateGame" />
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
            </AvalonForm>
            <AvalonButton label="Cancel" variant="secondary" @click="cancelTransfer" />
          </div>
        </AvalonCard>
      </div>
    </div>
  </div>
</template>
