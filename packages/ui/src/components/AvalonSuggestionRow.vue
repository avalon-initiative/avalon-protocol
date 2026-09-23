<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// A "people you may know" row — data comes in as props only,
// same "no fetch/token/route awareness here" invariant AvalonFriendRow
// documents. The only action is `add`, which the page maps to the normal
// POST /friends/requests flow; this component never sends a
// friend request itself.
import styles from '../styles/AvalonSuggestionRow.module.scss'
import AvalonAvatar from './AvalonAvatar.vue'
import type { AvalonSuggestionRowProps } from '../types/AvalonSuggestionRow.types'

defineProps<AvalonSuggestionRowProps>()
defineEmits<{ add: [] }>()
</script>

<template>
  <div :class="styles.row">
    <AvalonAvatar :src="avatarUrl" :name="displayName ?? identityId" size="md" />
    <span :class="styles.name">{{ displayName ?? identityId }}</span>
    <button
      :class="styles.add"
      type="button"
      :disabled="requested"
      @click="$emit('add')"
    >
      {{ requested ? 'Requested' : 'Add' }}
    </button>
  </div>
</template>
