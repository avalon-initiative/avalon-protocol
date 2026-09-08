// Edit-mode state for AvalonEditableField, kept out of the .vue file per
// this package's glue-only-script convention. The rule it implements: a
// field is a styled read-only value until Edit is pressed; then Enter, the
// Save button, or leaving the input commits (only if the value actually
// changed); Escape or Cancel reverts.
import { nextTick, ref } from 'vue'
import type { Ref } from 'vue'

export interface EditableFieldEmit {
  (event: 'save', value: string): void
  (event: 'cancel'): void
}

export function useEditableField(currentValue: () => string, emit: EditableFieldEmit) {
  const editing = ref(false)
  const draft = ref('')
  const input: Ref<HTMLInputElement | null> = ref(null)

  async function begin() {
    draft.value = currentValue()
    editing.value = true
    await nextTick()
    input.value?.focus()
    input.value?.select()
  }

  // Guarded on `editing` because blur and a Save click can both fire for
  // one edit — the second call must be a no-op, never a second save.
  function commit() {
    if (!editing.value) return
    editing.value = false
    const next = draft.value.trim()
    if (next !== currentValue()) emit('save', next)
  }

  function cancel() {
    if (!editing.value) return
    editing.value = false
    draft.value = currentValue()
    emit('cancel')
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === 'Enter') {
      event.preventDefault()
      commit()
    } else if (event.key === 'Escape') {
      event.preventDefault()
      cancel()
    }
  }

  return { editing, draft, input, begin, commit, cancel, onKeydown }
}
