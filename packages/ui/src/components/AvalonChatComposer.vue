<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// The character counter/over-cap state is derived inline in the template
// (modelValue.length against maxChars) rather than a computed — the same
// tiny-fallback-expression scale AvalonAvatar's name-initial logic uses,
// not a real validation rule (the app's own validateComposerBody owns
// that; this component just reflects the cap it's given).
import styles from '../styles/AvalonChatComposer.module.scss'
import type { AvalonChatComposerProps } from './AvalonChatComposer.types'

const props = withDefaults(defineProps<AvalonChatComposerProps>(), {
  sending: false,
  disabled: false,
})
const emit = defineEmits<{ 'update:modelValue': [value: string]; send: [] }>()

function onInput(event: Event) {
  emit('update:modelValue', (event.target as HTMLTextAreaElement).value)
}

function onSubmit() {
  if (props.modelValue.trim().length > 0 && props.modelValue.length <= props.maxChars) {
    emit('send')
  }
}
</script>

<template>
  <form :class="styles.composer" @submit.prevent="onSubmit">
    <textarea
      :class="styles.input"
      :value="modelValue"
      :disabled="disabled || sending"
      placeholder="Message the channel…"
      rows="2"
      @input="onInput"
      @keydown.enter.exact.prevent="onSubmit"
    />
    <div :class="styles.footer">
      <span :class="[styles.counter, modelValue.length > maxChars && styles.overCap]">
        {{ modelValue.length }} / {{ maxChars }}
      </span>
      <button
        :class="styles.send"
        type="submit"
        :disabled="disabled || sending || modelValue.trim().length === 0 || modelValue.length > maxChars"
      >
        {{ sending ? 'Sending…' : 'Send' }}
      </button>
    </div>
    <p v-if="error" :class="styles.error">{{ error }}</p>
  </form>
</template>
