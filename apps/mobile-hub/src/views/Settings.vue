<script setup lang="ts">
// Issue #60: the server URL is configurable at runtime rather than only at
// build time, same storage/behavior as apps/hub's network selector — see
// @avalon/api-client's client.ts (getServerUrl/setServerUrl). Changing it
// takes effect on next load, same as apps/hub's own reload-based
// convention, since an in-flight session's token has no meaning against a
// different server.
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonButton, AvalonCard, AvalonTextField } from '@avalon/ui'
import { getServerUrl, setServerUrl } from '@avalon/api-client'
import styles from '../styles/Settings.module.scss'

const router = useRouter()
const serverUrl = ref(getServerUrl())
const saved = ref(false)

function onSave() {
  setServerUrl(serverUrl.value.trim())
  saved.value = true
  // The Tauri webview reloads like any other browser view — the CSP's
  // `connect-src`/`img-src` origin is baked in at build time
  // (tauri.conf.json), not re-derived from this setting, so a server
  // switch across origins needs a relaunch, not just a reload, to take
  // effect against a *different* CSP. Same-origin-scheme changes (e.g. a
  // different port on 127.0.0.1) take effect on reload.
  window.location.reload()
}
</script>

<template>
  <main :class="styles.main">
    <AvalonCard title="Server" subtitle="Which avalon-server this app talks to.">
      <AvalonTextField
        v-model="serverUrl"
        label="Server URL"
        placeholder="http://127.0.0.1:8080"
      />
      <p v-if="saved" :class="styles.hint">Saved — reloading…</p>
      <div :class="styles.actions">
        <AvalonButton label="Save" variant="primary" @click="onSave" />
        <AvalonButton label="Back" variant="secondary" @click="router.back()" />
      </div>
    </AvalonCard>
  </main>
</template>
