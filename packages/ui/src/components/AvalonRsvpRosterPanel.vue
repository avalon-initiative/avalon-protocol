<script setup lang="ts">
// Convention: no <style> blocks, glue-only script. Who's going/maybe/can't
// go for one guild event (issue #248) — a thin wrapper around AvalonModal
// so it gets the same backdrop/panel/close treatment as every other modal
// in the Hub. One shared component reused from both the Events tab and the
// Calendar tab's selected-day list in apps/hub/src/views/Guild.vue, rather
// than each tab building its own roster UI.
import AvalonModal from './AvalonModal.vue'
import styles from '../styles/AvalonRsvpRosterPanel.module.scss'
import type { AvalonRsvpRosterPanelProps } from './AvalonRsvpRosterPanel.types'

withDefaults(defineProps<AvalonRsvpRosterPanelProps>(), {
  loading: false,
  error: '',
})
defineEmits<{ close: [] }>()
</script>

<template>
  <AvalonModal :open="open" :title="eventTitle" @close="$emit('close')">
    <p v-if="loading" :class="styles.status">Loading responses…</p>
    <p v-else-if="error" :class="styles.status">{{ error }}</p>
    <template v-else>
      <div v-for="group in groups" :key="group.status" :class="styles.group">
        <h3 :class="styles.groupLabel">{{ group.label }} ({{ group.names.length }})</h3>
        <p v-if="group.names.length === 0" :class="styles.empty">Nobody yet</p>
        <ul v-else :class="styles.names">
          <li v-for="name in group.names" :key="name">{{ name }}</li>
        </ul>
      </div>
    </template>
  </AvalonModal>
</template>
