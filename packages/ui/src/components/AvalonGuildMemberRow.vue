<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// Data comes in as props only — no fetch, token, or route awareness here
// (issue #24's invariant, matching AvalonFriendRow's pattern).
import styles from '../styles/AvalonGuildMemberRow.module.scss'
import AvalonAvatar from './AvalonAvatar.vue'
import AvalonPresenceBadge from './AvalonPresenceBadge.vue'
import AvalonRoleBadge from './AvalonRoleBadge.vue'
import type { AvalonGuildMemberRowProps } from './AvalonGuildMemberRow.types'

withDefaults(defineProps<AvalonGuildMemberRowProps>(), {
  canChangeRole: false,
  canKick: false,
})
defineEmits<{ 'change-role': []; kick: [] }>()
</script>

<template>
  <div :class="styles.row">
    <AvalonAvatar :src="avatarUrl" :name="displayName ?? identityId" size="md" />
    <span :class="styles.name">{{ displayName ?? identityId }}</span>
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
