<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files.
import styles from '../styles/AvalonChannelList.module.scss'
import type { AvalonChannelListProps } from './AvalonChannelList.types'

withDefaults(defineProps<AvalonChannelListProps>(), {
  canManage: false,
})
defineEmits<{ select: [id: string]; create: []; archive: [id: string] }>()
</script>

<template>
  <div :class="styles.list">
    <p v-if="channels.length === 0" :class="styles.empty">No channels yet.</p>
    <button
      v-for="channel in channels"
      :key="channel.id"
      type="button"
      :class="[styles.channel, channel.id === activeChannelId && styles.active]"
      @click="$emit('select', channel.id)"
    >
      <span :class="styles.hash">#</span>
      <span :class="styles.name">{{ channel.name }}</span>
      <span v-if="channel.archived" :class="styles.archivedTag">Archived</span>
      <button
        v-if="canManage && !channel.archived"
        type="button"
        :class="styles.archiveButton"
        @click.stop="$emit('archive', channel.id)"
      >
        Archive
      </button>
    </button>
    <button v-if="canManage" type="button" :class="styles.createButton" @click="$emit('create')">
      + New channel
    </button>
  </div>
</template>
