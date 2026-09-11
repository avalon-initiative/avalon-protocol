<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// Data comes in as props only — no fetch, token, or route awareness here
// (issue #18's invariant).
import styles from '../styles/AvalonFriendRow.module.scss'
import AvalonAvatar from './AvalonAvatar.vue'
import AvalonPresenceBadge from './AvalonPresenceBadge.vue'
import type { AvalonFriendRowProps } from './AvalonFriendRow.types'

defineProps<AvalonFriendRowProps>()
defineEmits<{ remove: []; message: [] }>()
</script>

<template>
  <div :class="styles.row">
    <AvalonAvatar :src="avatarUrl" :name="displayName ?? identityId" size="md" />
    <span :class="styles.name">{{ displayName ?? identityId }}</span>
    <AvalonPresenceBadge :status="status" />
    <button :class="styles.message" type="button" @click="$emit('message')">Message</button>
    <button :class="styles.remove" type="button" @click="$emit('remove')">Remove</button>
  </div>
</template>
