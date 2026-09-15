<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
import styles from '../styles/AvalonChatMessage.module.scss'
import AvalonAvatar from './AvalonAvatar.vue'
import type { AvalonChatMessageProps } from '../types/AvalonChatMessage.types'

withDefaults(defineProps<AvalonChatMessageProps>(), {
  canDelete: false,
  isOwn: false,
})
defineEmits<{ delete: [] }>()
</script>

<template>
  <div :class="[styles.message, isOwn ? styles.own : styles.other]">
    <div v-if="!isOwn" :class="styles.avatarWrap">
      <AvalonAvatar :name="authorDisplayName ?? authorId" size="sm" />
      <span
        v-if="presenceStatus"
        :class="[styles.presenceDot, styles[presenceStatus]]"
        :title="presenceStatus"
      />
    </div>
    <div :class="styles.bubble">
      <div :class="styles.meta">
        <span v-if="!isOwn" :class="styles.author">{{ authorDisplayName ?? authorId }}</span>
        <span :class="styles.time">{{ sentAtLabel }}</span>
      </div>
      <p :class="styles.body">{{ body }}</p>
    </div>
    <button v-if="canDelete" type="button" :class="styles.deleteButton" @click="$emit('delete')">
      Delete
    </button>
  </div>
</template>
