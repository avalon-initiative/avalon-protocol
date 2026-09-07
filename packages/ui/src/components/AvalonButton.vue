<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// Styling lives in the co-located .module.scss (CSS Modules); prop/behavior
// types live in the co-located .types.ts. This block stays glue-only.
import styles from './AvalonButton.module.scss'
import type { AvalonButtonProps } from './AvalonButton.types'

withDefaults(defineProps<AvalonButtonProps>(), {
  variant: 'primary',
})
</script>

<template>
  <button :class="[styles.avalonButton, styles[variant]]">{{ label }}</button>
</template>
