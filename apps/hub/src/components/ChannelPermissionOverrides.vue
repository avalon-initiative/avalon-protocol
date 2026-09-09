<script setup lang="ts">
// Issue #250: per-resource permission overrides, extending the existing
// Roles-tab permission matrix (Guild.vue) with a per-channel view of the
// same idea. A role's base permissions (set on the Roles tab) still apply
// everywhere by default; this panel lets a manage_roles holder grant or
// deny one permission for one role on this one channel specifically —
// server-side semantics (crates/server/src/guilds.rs::resolve_resource_permission):
// an explicit deny always beats a base grant, an explicit grant always
// beats a base absence.
//
// Also carries the announcement-only toggle (this ticket's end-to-end
// channel-organization feature): when set, `channel_post` becomes required
// to post in this channel, gated the same "any current member may post"
// default everywhere else — the override rows below are how a role earns
// (or loses) that ability for this specific channel.
//
// Convention: no <style> block, styling in the co-located .module.scss;
// script stays glue over the api client plus local load/error state.
import { computed, ref, watch } from 'vue'
import * as api from '../api/client'
import type { ChannelResponse, PermissionOverrideResponse, RoleResponse } from '../api/types'
import styles from './ChannelPermissionOverrides.module.scss'

const props = defineProps<{
  token: string
  guildId: string
  channel: ChannelResponse
  roles: RoleResponse[]
  // manage_roles: required to read/write overrides. manage_channels:
  // required to toggle announcement_only. A caller with only one of the
  // two still sees the panel, with the part they can't use disabled and
  // any attempt surfaced as a plain server-rejected error, not hidden.
  canManageRoles: boolean
  canManageChannels: boolean
}>()

const emit = defineEmits<{ updated: [] }>()

type OverrideState = 'inherit' | 'allow' | 'deny'

// The two permissions a channel-scoped override actually matters for —
// `event_manage` has no channel resource to attach to, and the other flat
// permissions (manage_guild/manage_roles/manage_members) are guild-wide
// by design (see docs/architecture/guilds.md).
const OVERRIDE_PERMISSIONS = ['channel_post', 'manage_channels'] as const

const overrides = ref<PermissionOverrideResponse[]>([])
const loading = ref(false)
const error = ref('')
const togglingAnnouncement = ref(false)
const pendingKey = ref('')

const nonOwnerRoles = computed(() => props.roles.filter((r) => r.name_index !== 0))

async function loadOverrides() {
  if (!props.canManageRoles) return
  loading.value = true
  error.value = ''
  try {
    overrides.value = await api.listPermissionOverrides(
      props.token,
      props.guildId,
      'channel',
      props.channel.id,
    )
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    loading.value = false
  }
}

watch(() => props.channel.id, loadOverrides, { immediate: true })

function overrideFor(roleIndex: number, permission: string): PermissionOverrideResponse | undefined {
  return overrides.value.find((o) => o.role_index === roleIndex && o.permission === permission)
}

function stateFor(roleIndex: number, permission: string): OverrideState {
  const existing = overrideFor(roleIndex, permission)
  if (!existing) return 'inherit'
  return existing.allow ? 'allow' : 'deny'
}

async function onToggleAnnouncementOnly() {
  togglingAnnouncement.value = true
  error.value = ''
  try {
    await api.updateChannel(props.token, props.guildId, props.channel.id, {
      name: props.channel.name,
      announcement_only: !props.channel.announcement_only,
    })
    emit('updated')
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    togglingAnnouncement.value = false
  }
}

async function onSetState(roleIndex: number, permission: string, state: OverrideState) {
  const key = `${roleIndex}:${permission}`
  pendingKey.value = key
  error.value = ''
  try {
    const existing = overrideFor(roleIndex, permission)
    if (state === 'inherit') {
      if (existing) await api.deletePermissionOverride(props.token, props.guildId, existing.id)
    } else {
      await api.setPermissionOverride(props.token, props.guildId, {
        role_index: roleIndex,
        resource_kind: 'channel',
        resource_id: props.channel.id,
        permission,
        allow: state === 'allow',
      })
    }
    await loadOverrides()
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    pendingKey.value = ''
  }
}

function onSelectChange(roleIndex: number, permission: string, event: Event) {
  const value = (event.target as HTMLSelectElement).value as OverrideState
  onSetState(roleIndex, permission, value)
}
</script>

<template>
  <div :class="styles.panel">
    <label :class="styles.announcementRow">
      <input
        type="checkbox"
        :checked="channel.announcement_only"
        :disabled="!canManageChannels || togglingAnnouncement"
        @change="onToggleAnnouncementOnly"
      />
      <span
        >Announcement-only — only roles allowed <code>channel_post</code> here (or granted it
        below) may post</span
      >
    </label>

    <p v-if="error" :class="styles.error">{{ error }}</p>

    <p v-if="!canManageRoles" :class="styles.hint">
      Per-role overrides require the manage_roles permission.
    </p>
    <p v-else-if="loading" :class="styles.hint">Loading overrides…</p>
    <table v-else :class="styles.table">
      <thead>
        <tr>
          <th>Role</th>
          <th v-for="permission in OVERRIDE_PERMISSIONS" :key="permission">{{ permission }}</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="role in nonOwnerRoles" :key="role.name_index">
          <td>{{ role.name }}</td>
          <td v-for="permission in OVERRIDE_PERMISSIONS" :key="permission">
            <select
              :value="stateFor(role.name_index, permission)"
              :disabled="pendingKey === `${role.name_index}:${permission}`"
              @change="onSelectChange(role.name_index, permission, $event)"
            >
              <option value="inherit">Inherit</option>
              <option value="allow">Allow</option>
              <option value="deny">Deny</option>
            </select>
          </td>
        </tr>
      </tbody>
    </table>
    <p v-if="canManageRoles && nonOwnerRoles.length === 0" :class="styles.hint">
      No roles other than owner exist yet — create one on the Roles tab first.
    </p>
  </div>
</template>
