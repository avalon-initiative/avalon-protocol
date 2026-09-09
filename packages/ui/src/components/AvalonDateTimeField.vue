<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files —
// all calendar-grid/date math lives in AvalonDateTimeField.state.ts, this
// file is glue over that composable. A fully custom popover calendar +
// hour/minute selects, not a native <input type="datetime-local"> —
// browser-native date/time widgets vary too much across browsers.
import styles from '../styles/AvalonDateTimeField.module.scss'
import type { AvalonDateTimeFieldProps } from './AvalonDateTimeField.types'
import { useDateTimeField } from './AvalonDateTimeField.state'

const props = defineProps<AvalonDateTimeFieldProps>()
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()

const {
  open,
  grid,
  monthLabel,
  parsed,
  popover,
  trigger,
  toggleOpen,
  close,
  onFocusOut,
  prevMonth,
  nextMonth,
  selectDay,
  setHour,
  setMinute,
  onTextInput,
} = useDateTimeField(() => props.modelValue, emit)

const hourOptions = Array.from({ length: 24 }, (_, i) => i)
const minuteOptions = [0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55]
</script>

<template>
  <div :class="styles.field" @focusout="onFocusOut">
    <span :class="styles.label">{{ label }}</span>
    <div :class="styles.control">
      <input
        :class="[styles.input, error ? styles.inputError : '']"
        type="text"
        placeholder="YYYY-MM-DDTHH:mm"
        :disabled="disabled"
        :value="modelValue"
        @input="onTextInput(($event.target as HTMLInputElement).value)"
      />
      <button
        ref="trigger"
        type="button"
        :class="styles.pickerButton"
        :disabled="disabled"
        aria-label="Open date and time picker"
        @click="toggleOpen"
      >
        📅
      </button>
    </div>
    <span v-if="error" :class="styles.error">{{ error }}</span>

    <div v-if="open" ref="popover" tabindex="-1" :class="styles.popover">
      <div :class="styles.popoverHeader">
        <button type="button" :class="styles.navButton" aria-label="Previous month" @click="prevMonth">
          &lsaquo;
        </button>
        <span :class="styles.monthLabel">{{ monthLabel }}</span>
        <button type="button" :class="styles.navButton" aria-label="Next month" @click="nextMonth">
          &rsaquo;
        </button>
      </div>

      <div :class="styles.weekdayRow">
        <span v-for="label in ['Su', 'Mo', 'Tu', 'We', 'Th', 'Fr', 'Sa']" :key="label" :class="styles.weekday">
          {{ label }}
        </span>
      </div>

      <div v-for="(week, wi) in grid" :key="wi" :class="styles.weekRow">
        <button
          v-for="cell in week"
          :key="cell.iso"
          type="button"
          :class="[
            styles.dayCell,
            !cell.inMonth && styles.dayOutside,
            parsed && cell.iso === `${parsed.year}-${String(parsed.month).padStart(2, '0')}-${String(cell.day).padStart(2, '0')}` && cell.inMonth
              ? styles.daySelected
              : '',
          ]"
          @click="selectDay(cell)"
        >
          {{ cell.day }}
        </button>
      </div>

      <div :class="styles.timeRow">
        <label :class="styles.timeField">
          <span :class="styles.timeLabel">Hour</span>
          <select :class="styles.timeSelect" :value="parsed?.hour ?? 0" @change="setHour(Number(($event.target as HTMLSelectElement).value))">
            <option v-for="h in hourOptions" :key="h" :value="h">{{ String(h).padStart(2, '0') }}</option>
          </select>
        </label>
        <label :class="styles.timeField">
          <span :class="styles.timeLabel">Minute</span>
          <select :class="styles.timeSelect" :value="parsed?.minute ?? 0" @change="setMinute(Number(($event.target as HTMLSelectElement).value))">
            <option v-for="m in minuteOptions" :key="m" :value="m">{{ String(m).padStart(2, '0') }}</option>
          </select>
        </label>
      </div>

      <button type="button" :class="styles.doneButton" @click="close">Done</button>
    </div>
  </div>
</template>
