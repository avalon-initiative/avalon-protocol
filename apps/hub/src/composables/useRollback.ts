// State for the post-compromise rollback view: the chosen window, the
// candidate list, and the two-step (confirm, then sign) reversal flow.
import { ref } from 'vue'
import type { RollbackCandidate } from '@avalon-initiative/protocol-sdk'
import { useSessionStore } from '../api/session'
import {
  errorCode,
  reverseRollbackEvent,
  rollbackCandidates,
  rollbackErrorMessage,
  toRfc3339Utc,
} from '../api/rollback'

export function useRollback() {
  const session = useSessionStore()

  const sinceLocal = ref('')
  const candidates = ref<RollbackCandidate[] | null>(null)
  // The exact string used for the listing; reversals reuse it unchanged.
  const activeSince = ref('')
  const loading = ref(false)
  const error = ref('')
  const success = ref('')
  const confirmingId = ref<string | null>(null)
  const reversingId = ref<string | null>(null)

  async function load(since: string) {
    const s = session.session
    if (!s) return
    const res = await rollbackCandidates(s, since)
    candidates.value = res.candidates
  }

  async function find() {
    error.value = ''
    success.value = ''
    confirmingId.value = null
    const since = toRfc3339Utc(sinceLocal.value)
    if (!since) {
      error.value = rollbackErrorMessage('INVALID_ROLLBACK_WINDOW')
      return
    }
    loading.value = true
    try {
      activeSince.value = since
      await load(since)
    } catch (e) {
      candidates.value = null
      error.value = rollbackErrorMessage(errorCode(e))
    } finally {
      loading.value = false
    }
  }

  function askConfirm(eventId: string) {
    confirmingId.value = eventId
  }

  function cancelConfirm() {
    confirmingId.value = null
  }

  async function confirmUndo(eventId: string) {
    const s = session.session
    if (!s) return
    error.value = ''
    success.value = ''
    reversingId.value = eventId
    try {
      await reverseRollbackEvent(s, eventId, activeSince.value)
      confirmingId.value = null
      await load(activeSince.value)
      success.value = 'Action undone.'
    } catch (e) {
      confirmingId.value = null
      error.value = rollbackErrorMessage(errorCode(e))
    } finally {
      reversingId.value = null
    }
  }

  return {
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
  }
}
