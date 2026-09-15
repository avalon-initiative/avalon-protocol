<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// Data comes in as props only — no fetch, token, or route awareness here
// (issue #24's invariant, matching AvalonFriendRow's pattern).
import { computed } from 'vue'
import styles from '../styles/AvalonGuildMemberRow.module.scss'
import AvalonAvatar from './AvalonAvatar.vue'
import AvalonPresenceBadge from './AvalonPresenceBadge.vue'
import AvalonRoleBadge from './AvalonRoleBadge.vue'
import type { AvalonGuildMemberRowProps } from '../types/AvalonGuildMemberRow.types'

const props = withDefaults(defineProps<AvalonGuildMemberRowProps>(), {
  canChangeRole: false,
  canKick: false,
})
defineEmits<{ 'change-role': []; kick: []; view: [] }>()

// #161's batch identity lookup resolves real display names now, wired in
// via apps/hub/src/api/guilds.ts. `displayName` can still be undefined for
// an edge case the lookup doesn't cover (e.g. a profile row missing for
// some reason) — fall back to a shortened id rather than a raw UUID wall,
// still obviously not a real name, just less jarring in a roster list.
function shortenIdentityId(id: string): string {
  return id.length > 12 ? `${id.slice(0, 8)}…` : id
}

const fallbackName = computed(() => props.displayName ?? shortenIdentityId(props.identityId))
</script>

<template>
  <div :class="styles.row">
    <button :class="styles.viewTrigger" type="button" @click="$emit('view')">
      <AvalonAvatar :src="avatarUrl" :name="displayName ?? identityId" size="md" />
      <span :class="styles.name" :title="identityId">{{ fallbackName }}</span>
    </button>
    <AvalonRoleBadge :name="roleName" :variant="roleVariant" />
    <AvalonPresenceBadge :status="status" />
    <div v-if="canChangeRole || canKick" :class="styles.actions">
      <button
        v-if="canChangeRole"
        :class="styles.actionButton"
        type="button"
        @click="$emit('change-role')"
      >
        Change role
      </button>
      <button
        v-if="canKick"
        :class="[styles.actionButton, styles.danger]"
        type="button"
        @click="$emit('kick')"
      >
        Kick
      </button>
    </div>
  </div>
</template>
