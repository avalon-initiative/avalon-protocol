<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files —
// styling lives in the co-located .module.scss, logic lives in .ts files.
import styles from './App.module.scss'
</script>

<template>
  <main :class="styles.main">
    <RouterView />
  </main>
</template>
