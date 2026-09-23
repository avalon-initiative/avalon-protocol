<script setup lang="ts">
// Lists what this identity did inside a compromise window and lets the owner
// undo each reversible action. Logic lives in useRollback.
import { AvalonButton, AvalonCard, AvalonDateTimeField } from '@avalon/ui'
import { useRollback } from '../composables/useRollback'
import page from '../styles/page.module.scss'
import styles from '../styles/Rollback.module.scss'

const {
  sinceLocal,
  candidates,
  loading,
  error,
  success,
  confirmingId,
  reversingId,
  find,
  askConfirm,
  cancelConfirm,
  confirmUndo,
} = useRollback()
</script>

<template>
  <div :class="page.page">
    <header :class="page.pageHeader">
      <h1 :class="page.title">Undo actions from a compromise</h1>
      <p :class="page.subtitle">
        Choose when you believe the compromise began. Actions your identity took between then and
        the moment your recovery completed are listed below.
      </p>
    </header>

    <AvalonCard title="Compromise window">
      <div :class="styles.form">
        <AvalonDateTimeField v-model="sinceLocal" label="Compromise began" />
        <div>
          <AvalonButton
            :label="loading ? 'Searching…' : 'Find actions'"
            variant="primary"
            :disabled="loading || !sinceLocal"
            @click="find"
          />
        </div>
      </div>
    </AvalonCard>

    <p v-if="error" :class="page.error" role="alert">{{ error }}</p>
    <p v-if="success" :class="styles.success" role="status">{{ success }}</p>

    <template v-if="candidates">
      <p v-if="candidates.length === 0" :class="page.empty">
        No actions were found in that window.
      </p>
      <ul v-else :class="styles.list">
        <li v-for="c in candidates" :key="c.eventId" :class="styles.item" data-testid="candidate">
          <strong>{{ c.summary }}</strong>
          <span :class="styles.meta">{{ c.kind }} · {{ c.occurredAt }}</span>
          <span v-if="c.alreadyReversed" :class="styles.status">Already undone</span>
          <span v-else-if="c.reversible" :class="styles.status">Can undo</span>
          <span v-else :class="styles.status">Cannot undo: {{ c.reason }}</span>

          <div v-if="c.reversible && !c.alreadyReversed" :class="styles.actions">
            <template v-if="confirmingId === c.eventId">
              <AvalonButton
                :label="reversingId === c.eventId ? 'Signing…' : 'Confirm undo'"
                variant="danger"
                :disabled="reversingId !== null"
                @click="confirmUndo(c.eventId)"
              />
              <AvalonButton label="Cancel" variant="secondary" @click="cancelConfirm" />
            </template>
            <AvalonButton v-else label="Undo" variant="secondary" @click="askConfirm(c.eventId)" />
          </div>
        </li>
      </ul>
    </template>
  </div>
</template>
