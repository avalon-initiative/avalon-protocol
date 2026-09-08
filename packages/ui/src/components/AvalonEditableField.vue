<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files —
// the edit-mode state machine lives in AvalonEditableField.state.ts.
// Read-only display by default; nothing here is interactive until the
// player presses Edit. `mousedown.prevent` on Save/Cancel keeps the input
// from blurring (and committing) before the button's click lands.
//
// Both rows use v-show, not v-if — the <input> stays mounted in the DOM at
// all times, just hidden (display:none, so it's genuinely non-interactive
// and out of the tab order) rather than created/destroyed on every click.
// Same read-only-until-Edit behavior either way; less DOM churn on a field
// a player might toggle repeatedly.
import styles from '../styles/AvalonEditableField.module.scss'
import type { AvalonEditableFieldProps } from './AvalonEditableField.types'
import { useEditableField } from './AvalonEditableField.state'

const props = withDefaults(defineProps<AvalonEditableFieldProps>(), {
  emptyText: 'Not set',
  saving: false,
})
const emit = defineEmits<{ save: [value: string]; cancel: [] }>()

const { editing, draft, input, begin, commit, cancel, onKeydown } = useEditableField(
  () => props.value,
  emit,
)
</script>

<template>
  <div :class="styles.field">
    <span :class="styles.label">{{ label }}</span>

    <div v-show="!editing" :class="styles.readRow">
      <span :class="[styles.value, value ? '' : styles.empty]">{{ value || emptyText }}</span>
      <button :class="styles.editButton" type="button" :disabled="saving" @click="begin">
        {{ saving ? 'Saving…' : 'Edit' }}
      </button>
    </div>

    <div v-show="editing" :class="styles.editRow">
      <input
        ref="input"
        v-model="draft"
        :class="styles.input"
        type="text"
        :placeholder="placeholder"
        @blur="commit"
        @keydown="onKeydown"
      />
      <button :class="styles.saveButton" type="button" @mousedown.prevent @click="commit">Save</button>
      <button :class="styles.cancelButton" type="button" @mousedown.prevent @click="cancel">
        Cancel
      </button>
    </div>

    <span v-if="error" :class="styles.error">{{ error }}</span>
  </div>
</template>
