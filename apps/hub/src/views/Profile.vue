<script setup lang="ts">
// The "you" page: profile, device setup/recovery (#134/#135), the device
// list, and log out. Everything is a styled read-only display until the
// player presses Edit (AvalonEditableField) or a button that starts an
// action — no open inputs sit on the page by default.
import { onMounted, onUnmounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import * as api from '../api/client'
import { AvalonApiError } from '../api/errors'
import { recoverSigningKey } from '../api/identity'
import {
  approveDeviceGrant,
  beginDeviceGrantRequest,
  finalizeApprovedGrant,
  findMySigningKeyId,
} from '../api/deviceGrants'
import { addPasskey, listPasskeys, renamePasskey, revokePasskey } from '../api/passkeys'
import type { DeviceGrantResponse, DeviceResponse, PasskeyResponse } from '../api/types'
import { loadSigningKey } from '../crypto/signingKey'
import { useSessionStore } from '../stores/session'
import {
  AvalonAvatar,
  AvalonButton,
  AvalonCard,
  AvalonEditableField,
  AvalonForm,
  AvalonTextField,
} from '@avalon/ui'
import page from './page.module.scss'
import styles from './Profile.module.scss'

// #135's device-grant flow polls rather than pushes (identity-management
// events don't need #136's presence-grade latency). Used both for a
// requesting device waiting on its own grant and for an already-set-up
// device refreshing the pending-approvals list.
const POLL_INTERVAL_MS = 5_000

const router = useRouter()
const session = useSessionStore()

const displayName = ref('')
const avatarUrl = ref('')
const handle = ref('')
const identityId = ref('')
const loading = ref(true)
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
    handle.value = profile.handle
    identityId.value = profile.identity_id
    hasSigningKey.value = loadSigningKey(profile.identity_id) !== null
    if (hasSigningKey.value) {
      await refreshDevicesAndPendingGrants()
    }
    await refreshPasskeys()
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

async function onLogout() {
  session.logout()
  await router.push({ name: 'login' })
}

// Each profile field saves on its own — PATCH /me takes any subset, and
// the display name and avatar are independently promised-durable (#86),
// so one edit is one change, one event.
type ProfileField = 'display_name' | 'avatar_url'
const savingField = ref<ProfileField | ''>('')
const fieldErrors = ref<Partial<Record<ProfileField, string>>>({})

async function saveProfileField(field: ProfileField, value: string) {
  if (!session.token) return
  fieldErrors.value = { ...fieldErrors.value, [field]: undefined }
  savingField.value = field
  try {
    const profile = await api.updateProfile(session.token, { [field]: value })
    displayName.value = profile.display_name
    avatarUrl.value = profile.avatar_url ?? ''
    handle.value = profile.handle
  } catch (e) {
    fieldErrors.value = {
      ...fieldErrors.value,
      [field]: e instanceof Error ? e.message : 'Something went wrong.',
    }
  } finally {
    savingField.value = ''
  }
}

const showRecovery = ref(false)
const recoveryPhrase = ref('')
const recovering = ref(false)
const recoveryError = ref('')

function onRecoverSigningKey() {
  recoveryError.value = ''
  recovering.value = true
  try {
    recoverSigningKey(identityId.value, recoveryPhrase.value.trim())
    hasSigningKey.value = true
    showRecovery.value = false
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
const renamingId = ref('')
const renameError = ref('')

async function refreshDevicesAndPendingGrants() {
  if (!session.token) return
  try {
    const [devices, grants] = await Promise.all([
      api.listDevices(session.token),
      api.listDeviceGrants(session.token, 'pending'),
    ])
    myDevices.value = devices
    pendingGrants.value = grants
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

// #145: every device (including the first one) can be renamed after the
// fact — from the field's own Edit, never an always-open input.
async function onRenameDevice(device: DeviceResponse, label: string) {
  if (!session.token) return
  renameError.value = ''
  renamingId.value = device.id
  try {
    await api.renameDevice(session.token, device.id, { label })
    await refreshDevicesAndPendingGrants()
  } catch (e) {
    renameError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    renamingId.value = ''
  }
}

// Multi-passkey registration (#200) — WebAuthn login credentials, kept
// deliberately separate from the signing-key device list above (see
// crates/server/src/passkeys.rs's module doc comment for why). Every
// identity has at least one passkey from account creation, so this list
// loads regardless of hasSigningKey/pendingRequest state.
const passkeys = ref<PasskeyResponse[]>([])
const addingPasskey = ref(false)
const addPasskeyError = ref('')
const renamingPasskeyId = ref('')
const renamePasskeyError = ref('')
const revokingPasskeyId = ref('')
const revokePasskeyError = ref('')

async function refreshPasskeys() {
  if (!session.token) return
  try {
    passkeys.value = await listPasskeys(session.token)
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

async function onAddPasskey() {
  if (!session.token) return
  addPasskeyError.value = ''
  addingPasskey.value = true
  try {
    await addPasskey(session.token, null)
    await refreshPasskeys()
  } catch (e) {
    addPasskeyError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    addingPasskey.value = false
  }
}

async function onRenamePasskey(passkey: PasskeyResponse, label: string) {
  if (!session.token) return
  renamePasskeyError.value = ''
  renamingPasskeyId.value = passkey.id
  try {
    await renamePasskey(session.token, passkey.id, label)
    await refreshPasskeys()
  } catch (e) {
    renamePasskeyError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    renamingPasskeyId.value = ''
  }
}

// Revoking the identity's last remaining passkey requires explicit
// confirmation (crates/server/src/passkeys.rs's own invariant, surfaced
// here as a 409 with AppError::LastPasskeyRequiresConfirmation) — a plain
// browser confirm() is enough for this milestone's UI, matching how little
// other chrome (no modal component in @avalon/ui yet) the rest of this page
// uses for destructive actions.
async function onRevokePasskey(passkey: PasskeyResponse) {
  if (!session.token) return
  revokePasskeyError.value = ''
  revokingPasskeyId.value = passkey.id
  try {
    await revokePasskey(session.token, passkey.id, false)
    await refreshPasskeys()
  } catch (e) {
    if (e instanceof AvalonApiError && e.status === 409) {
      // A 409 here always means the one thing it can mean for this
      // endpoint (crates/server/src/passkeys.rs's
      // LastPasskeyRequiresConfirmation) — messageForStatus's generic 409
      // text is about a different case (identity id collisions at account
      // creation) and isn't useful here, so this prompt is worded directly
      // rather than built from `e.message`.
      const confirmed = window.confirm(
        "This is your last remaining passkey — revoking it may lock you out of this identity if you have no other way to sign in. Revoke it anyway?",
      )
      if (confirmed) {
        try {
          await revokePasskey(session.token, passkey.id, true)
          await refreshPasskeys()
        } catch (retryError) {
          revokePasskeyError.value =
            retryError instanceof Error ? retryError.message : 'Something went wrong.'
        }
      }
    } else {
      revokePasskeyError.value = e instanceof Error ? e.message : 'Something went wrong.'
    }
  } finally {
    revokingPasskeyId.value = ''
  }
}
</script>

<template>
  <div v-if="!loading" :class="page.page">
    <header :class="styles.hero">
      <AvalonAvatar :src="avatarUrl || null" :name="displayName" size="xl" />
      <div :class="styles.heroText">
        <h1 :class="page.title">{{ displayName }}</h1>
        <p :class="styles.handle">{{ handle }}</p>
        <p :class="styles.identityId">{{ identityId }}</p>
      </div>
      <div :class="styles.heroActions">
        <AvalonButton label="Log out" variant="secondary" @click="onLogout" />
      </div>
    </header>
    <p v-if="error" :class="page.error">{{ error }}</p>

    <div :class="page.grid">
      <div :class="page.mainColumn">
        <AvalonCard title="Profile" subtitle="How other players see you.">
          <div :class="styles.fields">
            <AvalonEditableField
              label="Display name"
              :value="displayName"
              :saving="savingField === 'display_name'"
              :error="fieldErrors.display_name"
              @save="saveProfileField('display_name', $event)"
            />
            <AvalonEditableField
              label="Avatar URL"
              :value="avatarUrl"
              empty-text="No avatar"
              placeholder="https://…"
              :saving="savingField === 'avatar_url'"
              :error="fieldErrors.avatar_url"
              @save="saveProfileField('avatar_url', $event)"
            />
          </div>
        </AvalonCard>

        <AvalonCard v-if="!hasSigningKey" title="Set up this device">
          <section v-if="pendingRequest">
            <p :class="page.empty">
              Waiting for another device to approve this one
              <span v-if="pendingRequest.grant.status !== 'pending'">({{ pendingRequest.grant.status }})</span>.
            </p>
          </section>
          <section v-else :class="styles.stack">
            <p :class="page.empty">
              This device doesn't have a signing key for this identity yet. Request access from
              another device you're already signed in on, or recover from a saved phrase.
            </p>
            <p v-if="grantRequestError" :class="page.error">{{ grantRequestError }}</p>
            <div :class="styles.actions">
              <AvalonButton
                :label="requestingGrant ? 'Requesting…' : 'Request access from another device'"
                variant="primary"
                :disabled="requestingGrant"
                @click="onRequestDeviceGrant"
              />
              <AvalonButton
                v-show="!showRecovery"
                label="Recover with a phrase"
                variant="secondary"
                @click="showRecovery = true"
              />
            </div>
          </section>

          <div v-show="showRecovery">
            <h3 :class="styles.subheading">Recover your signing key</h3>
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
              <template #secondary-actions>
                <AvalonButton label="Cancel" variant="secondary" @click="showRecovery = false" />
              </template>
            </AvalonForm>
          </div>
        </AvalonCard>
      </div>

      <div :class="page.sideColumn">
        <AvalonCard
          title="Passkeys"
          subtitle="Any registered passkey can sign you in — none is more privileged than another. Register a second one from another device so losing one doesn't lock you out."
        >
          <p v-if="addPasskeyError" :class="page.error">{{ addPasskeyError }}</p>
          <p v-if="renamePasskeyError" :class="page.error">{{ renamePasskeyError }}</p>
          <p v-if="revokePasskeyError" :class="page.error">{{ revokePasskeyError }}</p>
          <ul :class="styles.list">
            <li v-for="passkey in passkeys" :key="passkey.id" :class="styles.device">
              <AvalonEditableField
                label="Passkey name"
                :value="passkey.label ?? ''"
                empty-text="Unlabeled passkey"
                :saving="renamingPasskeyId === passkey.id"
                @save="onRenamePasskey(passkey, $event)"
              />
              <div :class="styles.deviceActions">
                <span :class="styles.listDetail">Added {{ passkey.added_at }}</span>
                <AvalonButton
                  :label="revokingPasskeyId === passkey.id ? 'Revoking…' : 'Revoke'"
                  variant="danger"
                  :disabled="revokingPasskeyId === passkey.id"
                  @click="onRevokePasskey(passkey)"
                />
              </div>
            </li>
          </ul>
          <div :class="styles.actions">
            <AvalonButton
              :label="addingPasskey ? 'Waiting for your passkey…' : 'Add another passkey'"
              variant="primary"
              :disabled="addingPasskey"
              @click="onAddPasskey"
            />
          </div>
        </AvalonCard>

        <AvalonCard v-if="hasSigningKey && pendingGrants.length > 0" title="Devices waiting for your approval">
          <p v-if="approveError" :class="page.error">{{ approveError }}</p>
          <ul :class="styles.list">
            <li v-for="grant in pendingGrants" :key="grant.id" :class="styles.listRow">
              <span :class="styles.listText">
                <span :class="styles.listLabel">{{ grant.device_label ?? 'A device' }}</span>
                <span :class="styles.listDetail">requested access {{ grant.requested_at }}</span>
              </span>
              <AvalonButton
                :label="approvingGrantId === grant.id ? 'Approving…' : 'Approve'"
                variant="primary"
                :disabled="approvingGrantId === grant.id"
                @click="onApproveGrant(grant)"
              />
            </li>
          </ul>
        </AvalonCard>

        <AvalonCard
          v-if="hasSigningKey && myDevices.length > 0"
          title="Your devices"
          subtitle="To add another device, log into this identity there — with no signing key yet, it'll offer to request access, and the request will show up here for you to approve."
        >
          <p v-if="revokeError" :class="page.error">{{ revokeError }}</p>
          <p v-if="renameError" :class="page.error">{{ renameError }}</p>
          <ul :class="styles.list">
            <li v-for="device in myDevices" :key="device.id" :class="styles.device">
              <AvalonEditableField
                label="Device name"
                :value="device.label ?? ''"
                empty-text="Unlabeled device"
                :saving="renamingId === device.id"
                @save="onRenameDevice(device, $event)"
              />
              <div :class="styles.deviceActions">
                <span v-if="device.revoked_at" :class="styles.revoked">Revoked</span>
                <AvalonButton
                  v-if="!device.revoked_at"
                  :label="revokingId === device.id ? 'Revoking…' : 'Revoke'"
                  variant="danger"
                  :disabled="revokingId === device.id"
                  @click="onRevokeDevice(device)"
                />
              </div>
            </li>
          </ul>
        </AvalonCard>
      </div>
    </div>
  </div>
</template>
