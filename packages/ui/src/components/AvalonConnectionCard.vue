<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// One connected game (#83's GameBinding) + its active grants (#27), with a
// per-grant revoke button and a disconnect button. Emits capability
// strings / no payload — the caller (Connections.vue) owns the actual API
// calls and the slug this card belongs to.
import styles from '../styles/AvalonConnectionCard.module.scss'
import type { AvalonConnectionCardProps } from './AvalonConnectionCard.types'

defineProps<AvalonConnectionCardProps>()
defineEmits<{ 'revoke-grant': [capability: string]; disconnect: [] }>()
</script>

<template>
  <div :class="styles.card">
    <header :class="styles.header">
      <span :class="styles.name">{{ gameName }}</span>
      <span :class="styles.slug">{{ slug }}</span>
    </header>
    <p :class="styles.since">Connected since {{ establishedAt }}</p>

    <p v-if="grants.length === 0" :class="styles.empty">No active capability grants.</p>
    <ul v-else :class="styles.grants">
      <li v-for="grant in grants" :key="grant.capability" :class="styles.grant">
        <span :class="styles.grantText">{{ grant.description }}</span>
        <button
          :class="styles.revokeButton"
          type="button"
          @click="$emit('revoke-grant', grant.capability)"
        >
          Revoke
        </button>
      </li>
    </ul>

    <button :class="styles.disconnectButton" type="button" @click="$emit('disconnect')">
      Disconnect
    </button>
  </div>
</template>
