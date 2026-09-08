<script setup lang="ts">
// Identity/display-name editing — the "you" tab inside the shell (issue
// #130). The identity header (display name, identity id, logout) lives in
// HubShell.vue now, shown on every tab, not just this one.
import { onMounted, onUnmounted, ref } from 'vue'
import * as api from '../api/client'
import { recoverSigningKey } from '../api/identity'
import {
  approveDeviceGrant,
  beginDeviceGrantRequest,
  finalizeApprovedGrant,
  findMySigningKeyId,
} from '../api/deviceGrants'
import type { DeviceGrantResponse, DeviceResponse } from '../api/types'
import { loadSigningKey } from '../crypto/signingKey'
import { useSessionStore } from '../stores/session'
import { AvalonButton, AvalonForm, AvalonTextField } from '@avalon/ui'

// #135's device-grant flow polls rather than pushes (see the design's own
// reasoning: identity-management events don't need #136's presence-grade
// latency). Used both for a requesting device waiting on its own grant and
// for an already-set-up device refreshing the pending-approvals list.
const POLL_INTERVAL_MS = 5_000

const session = useSessionStore()

const displayName = ref('')
const avatarUrl = ref('')
const identityId = ref('')
const loading = ref(true)
const submitting = ref(false)
const error = ref('')

// #134: this device has no signing key for the current identity — either
// it's brand new, or storage was cleared. Recovering from a saved phrase
// is one fallback; requesting a grant from another trusted device (#135,
// the primary path) is the other. Either one flips this to true.
const hasSigningKey = ref(true)

let pollHandle: ReturnType<typeof setInterval> | undefined

onMounted(async () => {
  if (!session.token) return
  try {
    const profile = await api.getMe(session.token)
    displayName.value = profile.display_name
    avatarUrl.value = profile.avatar_url ?? ''
    identityId.value = profile.identity_id
    hasSigningKey.value = loadSigningKey(profile.identity_id) !== null
    if (hasSigningKey.value) {
      await refreshDevicesAndPendingGrants()
    }
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    loading.value = false
  }
  pollHandle = setInterval(() => {
    if (hasSigningKey.value) refreshDevicesAndPendingGrants()
    if (pendingRequest.value) pollMyGrant()
  }, POLL_INTERVAL_MS)
})

onUnmounted(() => {
  if (pollHandle) clearInterval(pollHandle)
})

const recoveryPhrase = ref('')
const recovering = ref(false)
const recoveryError = ref('')

function onRecoverSigningKey() {
  recoveryError.value = ''
  recovering.value = true
  try {
    recoverSigningKey(identityId.value, recoveryPhrase.value.trim())
    hasSigningKey.value = true
    recoveryPhrase.value = ''
    refreshDevicesAndPendingGrants()
  } catch (e) {
    recoveryError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    recovering.value = false
  }
}

// The #135 grant-request path — this device has no key and no phrase at
// hand, so it asks an already-trusted device to approve it instead.
const pendingRequest = ref<{ grant: DeviceGrantResponse; secretKey: Uint8Array } | null>(null)
const requestingGrant = ref(false)
const grantRequestError = ref('')

async function onRequestDeviceGrant() {
  if (!session.token) return
  grantRequestError.value = ''
  requestingGrant.value = true
  try {
    pendingRequest.value = await beginDeviceGrantRequest(session.token, null)
  } catch (e) {
    grantRequestError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    requestingGrant.value = false
  }
}

async function pollMyGrant() {
  if (!session.token || !pendingRequest.value) return
  try {
    const grant = await api.getDeviceGrant(session.token, pendingRequest.value.grant.id)
    if (grant.status === 'approved') {
      finalizeApprovedGrant(identityId.value, pendingRequest.value.secretKey)
      pendingRequest.value = null
      hasSigningKey.value = true
      await refreshDevicesAndPendingGrants()
    } else if (grant.status !== 'pending') {
      grantRequestError.value = `That request was ${grant.status}. Try again, or use a recovery phrase instead.`
      pendingRequest.value = null
    }
  } catch (e) {
    grantRequestError.value = e instanceof Error ? e.message : 'Something went wrong.'
    pendingRequest.value = null
  }
}

// The approval side — only reachable once this device already has a key,
// since approving requires signing with it.
const pendingGrants = ref<DeviceGrantResponse[]>([])
const myDevices = ref<DeviceResponse[]>([])
const approvingGrantId = ref('')
const approveError = ref('')
const revokingId = ref('')
const revokeError = ref('')

async function refreshDevicesAndPendingGrants() {
  if (!session.token) return
  try {
    const [devices, grants] = await Promise.all([
      api.listDevices(session.token),
      api.listDeviceGrants(session.token, 'pending'),
    ])
    myDevices.value = devices
    pendingGrants.value = grants
    seedRenameLabels(devices)
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

async function onApproveGrant(grant: DeviceGrantResponse) {
  if (!session.token) return
  approveError.value = ''
  approvingGrantId.value = grant.id
  try {
    const secretKey = loadSigningKey(identityId.value)
    if (!secretKey) throw new Error('This device has no signing key to approve with.')
    const myKeyId = await findMySigningKeyId(session.token, secretKey)
    if (!myKeyId) throw new Error("Couldn't find this device's own registered key.")
    await approveDeviceGrant(session.token, identityId.value, grant, myKeyId, secretKey)
    await refreshDevicesAndPendingGrants()
  } catch (e) {
    approveError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    approvingGrantId.value = ''
  }
}

async function onRevokeDevice(device: DeviceResponse) {
  if (!session.token) return
  revokeError.value = ''
  revokingId.value = device.id
  try {
    await api.revokeDevice(session.token, device.id)
    await refreshDevicesAndPendingGrants()
  } catch (e) {
    revokeError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    revokingId.value = ''
  }
}

// #145: every device (including the first one, previously unlabeled) can
// be renamed after the fact. Seeded from each device's current label on
// refresh, but only for a device not already being edited — an in-flight
// edit shouldn't get clobbered by the next poll tick.
const renameLabels = ref<Record<string, string>>({})
const renamingId = ref('')
const renameError = ref('')

function seedRenameLabels(devices: DeviceResponse[]) {
  for (const device of devices) {
    if (!(device.id in renameLabels.value)) {
      renameLabels.value[device.id] = device.label ?? ''
    }
  }
}

async function onRenameDevice(device: DeviceResponse) {
  if (!session.token) return
  const newLabel = (renameLabels.value[device.id] ?? '').trim()
  renameError.value = ''
  renamingId.value = device.id
  try {
    await api.renameDevice(session.token, device.id, { label: newLabel })
    await refreshDevicesAndPendingGrants()
  } catch (e) {
    renameError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    renamingId.value = ''
  }
}

async function onSubmit() {
  if (!session.token) return
  error.value = ''
  submitting.value = true
  try {
    const profile = await api.updateProfile(session.token, {
      display_name: displayName.value,
      avatar_url: avatarUrl.value,
    })
    displayName.value = profile.display_name
    avatarUrl.value = profile.avatar_url ?? ''
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    submitting.value = false
  }
}
</script>

<template>
  <section v-if="!loading">
    <AvalonForm submit-label="Save changes" :submitting="submitting" :error="error" @submit="onSubmit">
      <AvalonTextField v-model="displayName" label="Display name" />
      <AvalonTextField v-model="avatarUrl" label="Avatar URL" placeholder="https://…" />
    </AvalonForm>

    <section v-if="!hasSigningKey">
      <h2>Set up this device</h2>

      <section v-if="pendingRequest">
        <p>
          Waiting for another device to approve this one
          <span v-if="pendingRequest.grant.status !== 'pending'">({{ pendingRequest.grant.status }})</span>.
        </p>
      </section>
      <section v-else>
        <p>
          This device doesn't have a signing key for this identity yet. Request access from
          another device you're already signed in on, or recover from a saved phrase.
        </p>
        <AvalonForm
          submit-label="Request access from another device"
          :submitting="requestingGrant"
          :error="grantRequestError"
          @submit="onRequestDeviceGrant"
        />
      </section>

      <h3>Or recover your signing key</h3>
      <AvalonForm
        submit-label="Recover"
        :submitting="recovering"
        :error="recoveryError"
        @submit="onRecoverSigningKey"
      >
        <AvalonTextField
          v-model="recoveryPhrase"
          label="Recovery phrase"
          placeholder="twelve words separated by spaces"
        />
      </AvalonForm>
    </section>

    <section v-if="hasSigningKey && pendingGrants.length > 0">
      <h2>Devices waiting for your approval</h2>
      <p v-if="approveError">{{ approveError }}</p>
      <ul>
        <li v-for="grant in pendingGrants" :key="grant.id">
          {{ grant.device_label ?? 'A device' }} requested access at {{ grant.requested_at }}.
          <AvalonButton
            :label="approvingGrantId === grant.id ? 'Approving…' : 'Approve'"
            variant="secondary"
            :disabled="approvingGrantId === grant.id"
            @click="onApproveGrant(grant)"
          />
        </li>
      </ul>
    </section>

    <section v-if="hasSigningKey && myDevices.length > 0">
      <h2>Your devices</h2>
      <p>
        To add another device, log into this identity there — with no signing key yet, it'll
        offer to request access, and the request will show up here for you to approve.
      </p>
      <p v-if="revokeError">{{ revokeError }}</p>
      <p v-if="renameError">{{ renameError }}</p>
      <ul>
        <li v-for="device in myDevices" :key="device.id">
          <AvalonTextField
            v-model="renameLabels[device.id]"
            label="Device name"
            placeholder="Unlabeled device"
          />
          <span v-if="device.revoked_at">(revoked)</span>
          <AvalonButton
            :label="renamingId === device.id ? 'Saving…' : 'Rename'"
            variant="secondary"
            :disabled="renamingId === device.id"
            @click="onRenameDevice(device)"
          />
          <AvalonButton
            v-if="!device.revoked_at"
            :label="revokingId === device.id ? 'Revoking…' : 'Revoke'"
            variant="secondary"
            :disabled="revokingId === device.id"
            @click="onRevokeDevice(device)"
          />
        </li>
      </ul>
    </section>
  </section>
</template>
