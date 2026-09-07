<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files —
// styling lives in the co-located .module.scss, logic lives in .ts files.
import { AvalonButton } from '@avalon/ui'
import styles from './App.module.scss'

// Scaffold only: proves the hub -> @avalon/ui wiring works. Real identity
// setup / friends / guilds / achievements views come with the milestone-1
// vertical slice, once avalon-server exists to talk to.
</script>

<template>
  <main :class="styles.main">
    <h1>Avalon Hub</h1>
    <p>Your front door into Avalon Protocol.</p>
    <AvalonButton label="Create your Avalon identity" />
  </main>
</template>
