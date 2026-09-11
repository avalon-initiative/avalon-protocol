<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// One claim on the achievements view (issue #35): issuer, dates, and
// verification result rendered as facts (ADR #77 — the Hub renders
// verification results, it never computes or displays trust/rank). A
// revoked claim stays visible with both its issuance and revocation dates
// (#81/#85's durable-history invariant) — never removed, never collapsed
// to an empty slot.
import { ref } from 'vue'
import styles from '../styles/AvalonAchievementCard.module.scss'
import type { AvalonAchievementCardProps } from './AvalonAchievementCard.types'

defineProps<AvalonAchievementCardProps>()
defineEmits<{ 'view-issuer': [] }>()

// Collapsed by default — the issuance date is already shown up top, so
// history only needs expanding once there's more than that one entry to
// see (a revocation, eventually a reinstatement).
const historyOpen = ref(false)
</script>

<template>
  <div :class="styles.card">
    <div :class="styles.heading">
      <span :class="styles.name">{{ achievementName }}</span>
      <span :class="[styles.statusBadge, styles[status]]">{{ status }}</span>
    </div>

    <button :class="styles.issuerChip" type="button" @click="$emit('view-issuer')">
      {{ issuerName }}
    </button>
    <p :class="styles.issuedAt">Issued {{ issuedAt }}</p>
    <p v-if="status === 'invalid' && invalidReason" :class="styles.invalidReason">
      {{ invalidReason }}
    </p>

    <button
      v-if="history.length > 1"
      :class="styles.historyToggle"
      type="button"
      @click="historyOpen = !historyOpen"
    >
      {{ historyOpen ? 'Hide history' : 'Show history' }}
    </button>
    <ul v-if="historyOpen" :class="styles.history">
      <li v-for="(entry, index) in history" :key="index" :class="styles.historyEntry">
        <span :class="styles.historyEvent">{{ entry.event }}</span>
        <span :class="styles.historyAt">{{ entry.at }}</span>
        <span v-if="entry.reason" :class="styles.historyReason">{{ entry.reason }}</span>
      </li>
    </ul>
  </div>
</template>
