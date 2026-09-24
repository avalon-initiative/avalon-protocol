<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// `remove` covers both declining an incoming request and withdrawing an
// outgoing one — the server's own DELETE /friends/requests/:id endpoint
// already unifies those two actions (crates/server/src/friends.rs infers
// which from who's calling), so this component does too rather than
// inventing a distinction the API doesn't have.
import styles from '../styles/AvalonFriendRequestRow.module.scss'
import type { AvalonFriendRequestRowProps } from '../types/AvalonFriendRequestRow.types'

defineProps<AvalonFriendRequestRowProps>()
defineEmits<{ accept: []; remove: [] }>()
</script>

<template>
  <div :class="styles.row">
    <span :class="styles.name">{{ displayName ?? identityId }}</span>
    <span :class="styles.direction">{{ direction === 'incoming' ? 'wants to be friends' : 'pending' }}</span>
    <div :class="styles.actions">
      <button v-if="direction === 'incoming'" :class="styles.accept" type="button" @click="$emit('accept')">
        Accept
      </button>
      <button :class="styles.remove" type="button" @click="$emit('remove')">
        {{ direction === 'incoming' ? 'Decline' : 'Withdraw' }}
      </button>
    </div>
  </div>
</template>
