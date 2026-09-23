<script setup lang="ts">
// Issue #250: per-resource permission overrides, extending the existing
// Roles-tab permission matrix (Guild.vue) with a per-resource view of the
// same idea. A role's base permissions (set on the Roles tab) still apply
// everywhere by default; this panel lets a manage_roles holder grant or
// deny one permission for one role on this one resource specifically —
// server-side semantics (crates/server/src/guilds.rs::resolve_resource_permission
// for channel_post/manage_channels/event_manage, and the special
// resolve_view_permission for view/view_details): an explicit deny always
// beats a base grant, an explicit grant always beats a base absence — and
// for view/view_details specifically, an explicit view_details grant
// always implies view even under an explicit view deny.
//
// Issue #458 generalized this from a channel-only component
// (ChannelPermissionOverrides.vue) into this resource-kind-aware one, once
// events needed the exact same grid — the announcement-only toggle that
// used to live here moved to Guild.vue back in #276, so there was nothing
// channel-specific left in this component's own logic to begin with.
//
// Convention: no <style> block, styling in the sibling styles/.module.scss;
// script stays glue over the api client plus local load/error state.
import { computed, ref, watch } from 'vue'
import type { AccountSession, PermissionOverride, Role } from '@avalon-initiative/protocol-sdk'
import styles from '../styles/ResourcePermissionOverrides.module.scss'

const props = defineProps<{
  // #697/#698: setting/clearing an override is signature-required —
  // AccountSession.setPermissionOverride/deletePermissionOverride sign
  // automatically, so this is the only credential this component needs.
  session: AccountSession
  guildId: string
  resourceKind: 'channel' | 'event'
  resourceId: string
  roles: Role[]
  // Required to read/write overrides — the parent only renders this panel
  // once this is true, but it's still threaded through as a prop since
  // loadOverrides below re-checks it.
  canManageRoles: boolean
}>()

type OverrideState = 'inherit' | 'allow' | 'deny'

// The permissions each resource kind actually has something to attach to —
// the other flat permissions (manage_guild/manage_roles/manage_members)
// are guild-wide by design (see docs/architecture/guilds.md). `view`/
// `view_details` apply to both kinds identically.
const OVERRIDE_PERMISSIONS = computed(() =>
  props.resourceKind === 'channel'
    ? (['channel_post', 'manage_channels', 'view', 'view_details'] as const)
    : (['event_manage', 'view', 'view_details'] as const),
)

const overrides = ref<PermissionOverride[]>([])
const loading = ref(false)
const error = ref('')
const pendingKey = ref('')

const nonOwnerRoles = computed(() => props.roles.filter((r) => r.nameIndex !== 0))

async function loadOverrides() {
  if (!props.canManageRoles) return
  loading.value = true
  error.value = ''
  try {
    const result = await props.session.listPermissionOverrides(props.guildId, props.resourceKind, props.resourceId)
    overrides.value = Array.isArray(result) ? result : []
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    loading.value = false
  }
}

watch(() => props.resourceId, loadOverrides, { immediate: true })

function overrideFor(roleIndex: number, permission: string): PermissionOverride | undefined {
  return overrides.value.find((o) => o.roleIndex === roleIndex && o.permission === permission)
}

function stateFor(roleIndex: number, permission: string): OverrideState {
  const existing = overrideFor(roleIndex, permission)
  if (!existing) return 'inherit'
  return existing.allow ? 'allow' : 'deny'
}

async function onSetState(roleIndex: number, permission: string, state: OverrideState) {
  const key = `${roleIndex}:${permission}`
  pendingKey.value = key
  error.value = ''
  try {
    const existing = overrideFor(roleIndex, permission)
    if (state === 'inherit') {
      if (existing) {
        await props.session.deletePermissionOverride(props.guildId, existing.id)
      }
    } else {
      const allow = state === 'allow'
      await props.session.setPermissionOverride(
        props.guildId,
        roleIndex,
        props.resourceKind,
        props.resourceId,
        permission,
        allow,
      )
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
        <tr v-for="role in nonOwnerRoles" :key="role.nameIndex">
          <td>{{ role.name }}</td>
          <td v-for="permission in OVERRIDE_PERMISSIONS" :key="permission">
            <select
              :value="stateFor(role.nameIndex, permission)"
              :disabled="pendingKey === `${role.nameIndex}:${permission}`"
              @change="onSelectChange(role.nameIndex, permission, $event)"
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
