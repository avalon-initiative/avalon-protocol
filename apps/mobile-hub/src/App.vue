<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files —
// styling lives in the co-located .module.scss, logic lives in .ts files.
import { AvalonButton } from '@avalon/ui'
import styles from './App.module.scss'

// Companion-app scaffold (Proposal.md §22): guild chat/presence/friends
// reachable without a game open. Same @avalon/ui components as the web hub,
// wrapped in Tauri instead of a browser tab.
</script>

<template>
  <main :class="styles.main">
    <h1>Avalon</h1>
    <p>Your guild, wherever you are.</p>
    <AvalonButton label="Sign in" />
  </main>
</template>
