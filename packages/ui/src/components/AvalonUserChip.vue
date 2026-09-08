<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
import AvalonAvatar from './AvalonAvatar.vue'
import styles from '../styles/AvalonUserChip.module.scss'
import type { AvalonUserChipProps } from './AvalonUserChip.types'

withDefaults(defineProps<AvalonUserChipProps>(), {
  avatarSrc: null,
})
</script>

<template>
  <div :class="styles.chip">
    <AvalonAvatar :src="avatarSrc" :name="name" size="md" />
    <div :class="styles.text">
      <span :class="styles.name">{{ name }}</span>
      <span v-if="detail" :class="styles.detail">{{ detail }}</span>
    </div>
  </div>
</template>
