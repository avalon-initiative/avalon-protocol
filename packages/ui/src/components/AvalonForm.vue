<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
import styles from '../styles/AvalonForm.module.scss'
import type { AvalonFormProps } from './AvalonForm.types'

withDefaults(defineProps<AvalonFormProps>(), {
  submitting: false,
})

defineEmits<{ submit: [] }>()
</script>

<template>
  <form :class="styles.form" @submit.prevent="$emit('submit')">
    <slot />
    <p v-if="error" :class="styles.formError">{{ error }}</p>
    <button :class="styles.submit" type="submit" :disabled="submitting">
      {{ submitting ? 'Please wait…' : submitLabel }}
    </button>
  </form>
</template>
