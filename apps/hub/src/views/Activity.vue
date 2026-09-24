<script setup lang="ts">
// "What does the network know about me" — the caller's own
// protocol event history, read straight off the ledger
// (crates/server/src/handlers.rs's my_history) rather than a summarized
// digest, so a fresh identity's single `identity.created` entry and a busy
// identity's full history render the same way, just with more rows.
import { onMounted, onUnmounted, ref } from 'vue'
import { AvalonCard, AvalonIcon } from '@avalon-initiative/common-ui'
import { formatActivityTimestamp, summarizeActivityEntry } from '../api/activityFeed'
import type { HistoryEntry } from '@avalon-initiative/protocol-sdk'
import { useSessionStore } from '../api/session'
import page from '../styles/page.module.scss'
import styles from '../styles/Activity.module.scss'

// Issue #389: this view used to load once on mount and never refresh —
// polling on the same interval useConversations/useGuildChat already
// established for milestone 1 (no WebSocket needed) rather than adding a
// third, different cadence.
const POLL_INTERVAL_MS = 15_000

const session = useSessionStore()

const entries = ref<HistoryEntry[]>([])
const loading = ref(true)
const error = ref('')

let pollHandle: ReturnType<typeof setInterval> | undefined

async function refresh() {
  const s = session.session
  if (!s) return
  try {
    const result = await s.history()
    entries.value = Array.isArray(result) ? result : []
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

onMounted(async () => {
  await refresh()
  loading.value = false
  pollHandle = setInterval(refresh, POLL_INTERVAL_MS)
})

onUnmounted(() => {
  if (pollHandle) clearInterval(pollHandle)
})
</script>

<template>
  <div v-if="!loading" :class="page.page">
    <header :class="page.pageHeader">
      <h1 :class="page.title">Activity</h1>
      <p :class="page.subtitle">Everything the network has recorded about your identity.</p>
    </header>
    <AvalonCard :title="`Your history (${entries.length})`">
      <p v-if="error" :class="page.error">{{ error }}</p>
      <p v-else-if="entries.length === 0" :class="page.empty">
        No activity yet — your history starts as soon as the network processes your first event.
      </p>
      <ul v-else :class="styles.feed">
        <li v-for="entry in entries" :key="entry.eventId" :class="styles.entry">
          <span :class="styles.icon"><AvalonIcon name="activity" :size="16" /></span>
          <span :class="styles.summary">{{ summarizeActivityEntry(entry) }}</span>
          <time :class="styles.timestamp" :datetime="entry.timestamp">
            {{ formatActivityTimestamp(entry.timestamp) }}
          </time>
        </li>
      </ul>
    </AvalonCard>
  </div>
</template>
