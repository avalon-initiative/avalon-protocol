<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
// Purely presentational: takes the current query/sort as props, emits
// changes back up. No fetching, no filtering/sorting logic — that stays in
// the page/composable that owns the actual list.
import styles from '../styles/AvalonFilterBar.module.scss'
import type { AvalonFilterBarProps } from './AvalonFilterBar.types'

withDefaults(defineProps<AvalonFilterBarProps>(), {
  placeholder: 'Search…',
  label: 'Search',
})

defineEmits<{ 'update:query': [value: string]; 'update:sortValue': [value: string] }>()
</script>

<template>
  <div :class="styles.bar">
    <label :class="styles.field">
      <span :class="styles.label">{{ label }}</span>
      <input
        :class="styles.input"
        type="text"
        :placeholder="placeholder"
        :value="query"
        @input="$emit('update:query', ($event.target as HTMLInputElement).value)"
      />
    </label>
    <label v-if="sortOptions" :class="styles.field">
      <span :class="styles.label">Sort</span>
      <select
        :class="styles.select"
        :value="sortValue"
        @change="$emit('update:sortValue', ($event.target as HTMLSelectElement).value)"
      >
        <option v-for="option in sortOptions" :key="option.value" :value="option.value">
          {{ option.label }}
        </option>
      </select>
    </label>
  </div>
</template>
