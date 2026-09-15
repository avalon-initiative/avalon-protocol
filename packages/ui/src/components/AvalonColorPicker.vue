<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// Styling lives in src/styles/ (CSS Modules); prop/behavior types live in
// the co-located .types.ts. A visual picker (the browser's native color
// wheel, via <input type="color">) plus a manual hex field, kept in sync
// both ways — issue #451.
import { computed } from 'vue'
import styles from '../styles/AvalonColorPicker.module.scss'
import type { AvalonColorPickerProps } from '../types/AvalonColorPicker.types'

const props = defineProps<AvalonColorPickerProps>()
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()

const isValidHex = (value: string) => /^#[0-9a-fA-F]{6}$/.test(value)

// <input type="color"> requires a well-formed 6-digit hex at all times —
// falls back to a neutral placeholder while the text field holds an
// incomplete/invalid value rather than fighting the browser's own
// validation.
const swatchValue = computed(() => (isValidHex(props.modelValue) ? props.modelValue : '#888888'))

function onSwatchInput(event: Event) {
  emit('update:modelValue', (event.target as HTMLInputElement).value)
}

function onHexInput(event: Event) {
  emit('update:modelValue', (event.target as HTMLInputElement).value)
}
</script>

<template>
  <div :class="styles.field">
    <span :class="styles.label">{{ label }}</span>
    <div :class="styles.row">
      <input
        :class="styles.swatch"
        type="color"
        :value="swatchValue"
        :disabled="disabled"
        @input="onSwatchInput"
      />
      <input
        :class="[styles.hexInput, error ? styles.inputError : '']"
        type="text"
        placeholder="#a1b2c3"
        maxlength="7"
        :value="modelValue"
        :disabled="disabled"
        @input="onHexInput"
      />
    </div>
    <span v-if="error" :class="styles.error">{{ error }}</span>
  </div>
</template>
