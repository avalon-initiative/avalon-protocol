<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// A single requested capability in the integrator-connect consent view (#27):
// wire capability string, its plain-language description, and a checkbox.
// The caller owns which capabilities are checked (the "no approve all"
// invariant lives one level up, in whichever view/composable tracks the
// checked set) — this component only reports toggles, never defaults
// itself to checked.
import styles from '../styles/AvalonCapabilityConsentRow.module.scss'
import type { AvalonCapabilityConsentRowProps } from '../types/AvalonCapabilityConsentRow.types'

defineProps<AvalonCapabilityConsentRowProps>()
defineEmits<{ 'update:checked': [value: boolean] }>()
</script>

<template>
  <label :class="styles.row">
    <input
      :class="styles.checkbox"
      type="checkbox"
      :checked="checked"
      @change="$emit('update:checked', ($event.target as HTMLInputElement).checked)"
    />
    <span :class="styles.text">
      <span :class="styles.capability">{{ capability }}</span>
      <span :class="styles.description">{{ description }}</span>
    </span>
  </label>
</template>
