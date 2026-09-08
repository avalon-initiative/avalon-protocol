<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
import styles from '../styles/AvalonChatMessage.module.scss'
import AvalonAvatar from './AvalonAvatar.vue'
import type { AvalonChatMessageProps } from './AvalonChatMessage.types'

withDefaults(defineProps<AvalonChatMessageProps>(), {
  canDelete: false,
})
defineEmits<{ delete: [] }>()
</script>

<template>
  <div :class="styles.message">
    <AvalonAvatar :name="authorDisplayName ?? authorId" size="sm" />
    <div :class="styles.content">
      <div :class="styles.meta">
        <span :class="styles.author">{{ authorDisplayName ?? authorId }}</span>
        <span :class="styles.time">{{ sentAtLabel }}</span>
      </div>
      <p :class="styles.body">{{ body }}</p>
    </div>
    <button v-if="canDelete" type="button" :class="styles.deleteButton" @click="$emit('delete')">
      Delete
    </button>
  </div>
</template>
