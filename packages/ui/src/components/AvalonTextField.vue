<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// Styling lives in src/styles/ (CSS Modules); prop/behavior types live in
// the co-located .types.ts. This block stays glue-only.
import styles from '../styles/AvalonTextField.module.scss'
import type { AvalonTextFieldProps } from '../types/AvalonTextField.types'

withDefaults(defineProps<AvalonTextFieldProps>(), {
  type: 'text',
})

defineEmits<{ 'update:modelValue': [value: string] }>()
</script>

<template>
  <label :class="styles.field">
    <span :class="styles.label">{{ label }}</span>
    <input
      :class="[styles.input, error ? styles.inputError : '']"
      :type="type"
      :placeholder="placeholder"
      :disabled="disabled"
      :maxlength="maxlength"
      :value="modelValue"
      @input="$emit('update:modelValue', ($event.target as HTMLInputElement).value)"
    />
    <span v-if="error" :class="styles.error">{{ error }}</span>
  </label>
</template>
