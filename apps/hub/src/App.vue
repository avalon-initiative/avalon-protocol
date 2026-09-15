<script setup lang="ts">
// Convention: no <style> blocks and no non-trivial logic in .vue files —
// styling lives in the sibling .module.scss under styles/, logic lives in .ts files.
import styles from './styles/App.module.scss'
</script>

<template>
  <main :class="styles.main">
    <RouterView />
  </main>
</template>
