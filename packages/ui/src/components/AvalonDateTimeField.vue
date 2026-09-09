<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files —
// all calendar-grid/date math lives in AvalonDateTimeField.state.ts, this
// file is glue over that composable. A fully custom popover calendar
// (month/year as selects, no prev/next-arrow-only nav) with 12-hour
// HH:MM entry + AM/PM beside it — not a native <input type="datetime-local">,
// browser-native date/time widgets vary too much across browsers.
import styles from '../styles/AvalonDateTimeField.module.scss'
import type { AvalonDateTimeFieldProps } from './AvalonDateTimeField.types'
import { MONTH_NAMES, WEEKDAY_LABELS, useDateTimeField } from './AvalonDateTimeField.state'

const props = defineProps<AvalonDateTimeFieldProps>()
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()

const {
  open,
  viewYear,
  viewMonth,
  years,
  grid,
  selectedIso,
  popover,
  trigger,
  hour12Text,
  minuteText,
  period,
  toggleOpen,
  close,
  onFocusOut,
  setViewYear,
  setViewMonth,
  selectDay,
  setHour12Text,
  setMinuteText,
  setPeriod,
  onTextInput,
} = useDateTimeField(() => props.modelValue, emit)
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
    <span :class="styles.tzHint">Times are shown and entered in your device's local timezone.</span>

    <div v-if="open" ref="popover" tabindex="-1" :class="styles.popover">
      <div :class="styles.popoverLayout">
        <div :class="styles.calendarColumn">
          <div :class="styles.popoverHeader">
            <select
              :class="styles.rollSelect"
              :value="viewMonth"
              aria-label="Month"
              @change="setViewMonth(Number(($event.target as HTMLSelectElement).value))"
            >
              <option v-for="(name, i) in MONTH_NAMES" :key="name" :value="i + 1">{{ name }}</option>
            </select>
            <select
              :class="styles.rollSelect"
              :value="viewYear"
              aria-label="Year"
              @change="setViewYear(Number(($event.target as HTMLSelectElement).value))"
            >
              <option v-for="y in years" :key="y" :value="y">{{ y }}</option>
            </select>
          </div>

          <div :class="styles.weekdayRow">
            <span v-for="label in WEEKDAY_LABELS" :key="label" :class="styles.weekday">{{ label }}</span>
          </div>

          <div v-for="(week, wi) in grid" :key="wi" :class="styles.weekRow">
            <button
              v-for="cell in week"
              :key="cell.iso"
              type="button"
              :class="[
                styles.dayCell,
                !cell.inMonth && styles.dayOutside,
                cell.inMonth && cell.iso === selectedIso ? styles.daySelected : '',
              ]"
              @click="selectDay(cell)"
            >
              {{ cell.day }}
            </button>
          </div>
        </div>

        <div :class="styles.timeColumn">
          <span :class="styles.timeColumnLabel">Time</span>
          <div :class="styles.timeEntry">
            <input
              :class="styles.timeInput"
              type="text"
              inputmode="numeric"
              maxlength="2"
              aria-label="Hour"
              :value="hour12Text"
              @input="setHour12Text(($event.target as HTMLInputElement).value)"
            />
            <span :class="styles.timeColon">:</span>
            <input
              :class="styles.timeInput"
              type="text"
              inputmode="numeric"
              maxlength="2"
              aria-label="Minute"
              :value="minuteText"
              @input="setMinuteText(($event.target as HTMLInputElement).value)"
            />
          </div>
          <div :class="styles.periodToggle">
            <button
              type="button"
              :class="[styles.periodButton, period === 'AM' && styles.periodActive]"
              @click="setPeriod('AM')"
            >
              AM
            </button>
            <button
              type="button"
              :class="[styles.periodButton, period === 'PM' && styles.periodActive]"
              @click="setPeriod('PM')"
            >
              PM
            </button>
          </div>
        </div>
      </div>

      <button type="button" :class="styles.doneButton" @click="close">Done</button>
    </div>
  </div>
</template>
