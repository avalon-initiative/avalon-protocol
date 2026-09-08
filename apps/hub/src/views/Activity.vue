<script setup lang="ts">
// "What does the network know about me" (issue #121) — the caller's own
// protocol event history, read straight off the ledger
// (crates/server/src/handlers.rs's my_history) rather than a summarized
// digest, so a fresh identity's single `identity.created` entry and a busy
// identity's full history render the same way, just with more rows.
import { onMounted, ref } from 'vue'
import { getMyHistory } from '../api/client'
import { formatActivityTimestamp, summarizeActivityEntry } from '../api/activityFeed'
import type { HistoryEntryResponse } from '../api/types'
import { useSessionStore } from '../stores/session'
import styles from './Activity.module.scss'

const session = useSessionStore()

const entries = ref<HistoryEntryResponse[]>([])
const loading = ref(true)
const error = ref('')

onMounted(async () => {
  if (!session.token) return
  try {
    entries.value = await getMyHistory(session.token)
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    loading.value = false
  }
})
</script>

<template>
  <section v-if="!loading">
    <p v-if="error">{{ error }}</p>
    <p v-else-if="entries.length === 0">
      No activity yet — your history starts as soon as the network processes your first event.
    </p>
    <ul v-else :class="styles.feed">
      <li v-for="entry in entries" :key="entry.event_id" :class="styles.entry">
        <span :class="styles.summary">{{ summarizeActivityEntry(entry) }}</span>
        <time :class="styles.timestamp" :datetime="entry.timestamp">
          {{ formatActivityTimestamp(entry.timestamp) }}
        </time>
      </li>
    </ul>
  </section>
</template>
