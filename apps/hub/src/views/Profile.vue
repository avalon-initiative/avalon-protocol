<script setup lang="ts">
// The "you" page: profile, device setup/recovery (#134/#135), the device
// list, and log out. Everything is a styled read-only display until the
// user presses Edit (AvalonEditableField) or a button that starts an
// action — no open inputs sit on the page by default.
import { computed, onMounted, onUnmounted, ref } from 'vue'
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
import {
  approveRecoveryRequest,
  cancelRecoveryRequest,
  getGuardianOf,
  getGuardianRequests,
  getGuardians,
  getMyRecoveryStatus,
  resignAsGuardian,
  setGuardians,
} from '../api/recovery'
import type {
  DeviceGrantResponse,
  DeviceResponse,
  Genre,
  GuardianOfSummary,
  GuardianRequestSummary,
  PasskeyResponse,
  RecoveryRequestResponse,
} from '../api/types'
import { listFriendsWithPresence, type Friend } from '../api/friends'
import { listBlockedUsersWithNames, type BlockedUser } from '../api/blocks'
import { markGuardianOfSeen } from '../api/notifications'
import { useMyGuilds } from '../composables/useMyGuilds'
import { loadSigningKey } from '../crypto/signingKey'
import { useSessionStore } from '../stores/session'
import { shouldShowSinglePasskeyWarning } from '../utils/singlePasskeyWarning'
import { listIanaTimezones } from '../utils/timezones'
import { isIdentityId } from '../utils/identity'
import {
  AvalonAvatar,
  AvalonButton,
  AvalonCard,
  AvalonColorPicker,
  AvalonEditableField,
  AvalonForm,
  AvalonTextField,
  AvalonWarningBanner,
} from '@avalon/ui'
import page from '../styles/page.module.scss'
import styles from '../styles/Profile.module.scss'

// #135's device-grant flow polls rather than pushes (identity-management
// events don't need #136's presence-grade latency). Used both for a
// requesting device waiting on its own grant and for an already-set-up
// device refreshing the pending-approvals list.
const POLL_INTERVAL_MS = 5_000

const router = useRouter()
const session = useSessionStore()

const displayName = ref('')
const avatarUrl = ref('')
const identityId = ref('')
const loading = ref(true)
const error = ref('')

// Issue #155's self-description fields — the server has supported these
// since #155, but nothing in the Hub read or wrote them until #277.
const bio = ref('')
const pronouns = ref('')

// Issue #372's expanded self-description fields.
const bannerUrl = ref('')
const status = ref('')
const timezone = ref('')
const themeColor = ref('')
const location = ref('')

// A self-chosen pointer to one of this identity's own current guild
// memberships (no ticket — see identity::Profile::main_guild's doc
// comment). `mainGuild` is the raw, explicit value (empty means unset);
// `effectiveMainGuild` is what the server actually resolves it to (falling
// back to the earliest-joined membership) and is display-only — there's no
// way to "save" a default, only to explicitly pick a guild or clear back
// to it. The dropdown is populated from the same `useMyGuilds` composable
// the Guilds.vue landing page already uses, not a fresh fetch — it must
// only ever offer guilds this identity is actually a member of.
const { guilds: myGuilds } = useMyGuilds()
const mainGuild = ref('')
const effectiveMainGuild = ref('')
const savingMainGuild = ref(false)
const mainGuildError = ref('')

// `links` is a small fixed-size list (issue #372), same "saves as one
// explicit action" shape favorite_genres already uses below rather than a
// per-field save — up to MAX_LINKS text inputs, blank slots trimmed out on
// save.
const MAX_LINKS = 5
const links = ref<string[]>([])
const savingLinks = ref(false)
const linksError = ref('')

// Issue #155's closed genre vocabulary (crates/protocol/src/identity.rs's
// Genre::ALL) — a fixed picker, not free text, same "small controlled
// vocabulary" reasoning the server enforces on write.
const GENRE_OPTIONS: { value: Genre; label: string }[] = [
  { value: 'action', label: 'Action' },
  { value: 'adventure', label: 'Adventure' },
  { value: 'rpg', label: 'RPG' },
  { value: 'strategy', label: 'Strategy' },
  { value: 'simulation', label: 'Simulation' },
  { value: 'puzzle', label: 'Puzzle' },
  { value: 'racing', label: 'Racing' },
  { value: 'sports', label: 'Sports' },
  { value: 'horror', label: 'Horror' },
  { value: 'sandbox', label: 'Sandbox' },
  { value: 'mmo', label: 'MMO' },
  { value: 'shooter', label: 'Shooter' },
  { value: 'platformer', label: 'Platformer' },
  { value: 'party', label: 'Party' },
]
const MAX_FAVORITE_GENRES = 5
const selectedGenres = ref<Set<Genre>>(new Set())
const savingGenres = ref(false)
const genresError = ref('')

// Issue #205's opt-in global search toggle — off by default. A
// first-class control on this page (not tucked into a settings submenu),
// with `discoverable` doubling as its own "you are currently publicly
// searchable" indicator: it's always the server's current value, never
// assumed from the last click.
const discoverable = ref(false)
const savingDiscoverable = ref(false)
const discoverableError = ref('')

// Issue #87 — who can see this identity's presence status, same fixed
// Visibility vocabulary the guild roster-visibility control below uses.
const VISIBILITY_OPTIONS = [
  { value: 'public', label: 'Anyone' },
  { value: 'authenticated_only', label: 'Any signed-in user' },
  { value: 'friends', label: 'Friends only' },
  { value: 'guild_members', label: 'Guild members only' },
  { value: 'private', label: 'Only me' },
]
const presenceVisibility = ref('public')
const savingPresenceVisibility = ref(false)
const presenceVisibilityError = ref('')

async function onSavePresenceVisibility() {
  if (!session.token) return
  presenceVisibilityError.value = ''
  savingPresenceVisibility.value = true
  try {
    const profile = await api.updateProfile(session.token, {
      presence_visibility: presenceVisibility.value,
    })
    presenceVisibility.value = profile.presence_visibility
  } catch (e) {
    presenceVisibilityError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingPresenceVisibility.value = false
  }
}

// Issue #97/#460 — the caller's own outgoing blocks. Never lists who has
// blocked the caller (crates/server/src/blocks.rs's own invariant).
const blockedUsers = ref<BlockedUser[]>([])
const blockedUsersError = ref('')
const unblockingId = ref<string | null>(null)
const blockByIdInput = ref('')
const blockByIdError = ref('')
const blockingById = ref(false)

async function refreshBlockedUsers() {
  if (!session.token) return
  try {
    blockedUsers.value = await listBlockedUsersWithNames(session.token)
  } catch (e) {
    blockedUsersError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

async function onUnblock(identityId: string) {
  if (!session.token) return
  blockedUsersError.value = ''
  unblockingId.value = identityId
  try {
    await api.removeBlock(session.token, identityId)
    await refreshBlockedUsers()
  } catch (e) {
    blockedUsersError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    unblockingId.value = null
  }
}

// Accepts either a raw identity id or a display_name handle (#128, #510),
// same convention Friends.vue's onAddFriend already uses for the
// analogous "add by id/handle" flow.
async function onBlockById() {
  if (!session.token) return
  blockByIdError.value = ''
  blockingById.value = true
  try {
    const input = blockByIdInput.value.trim()
    const identityId = isIdentityId(input)
      ? input
      : (await api.resolveHandle(session.token, input)).identity_id
    await api.createBlock(session.token, { identity_id: identityId })
    blockByIdInput.value = ''
    await refreshBlockedUsers()
  } catch (e) {
    blockByIdError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    blockingById.value = false
  }
}

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
    bio.value = profile.bio ?? ''
    pronouns.value = profile.pronouns ?? ''
    bannerUrl.value = profile.banner_url ?? ''
    status.value = profile.status ?? ''
    timezone.value = profile.timezone ?? ''
    themeColor.value = profile.theme_color ?? ''
    location.value = profile.location ?? ''
    mainGuild.value = profile.main_guild ?? ''
    effectiveMainGuild.value = profile.effective_main_guild ?? ''
    links.value = profile.links?.length ? [...profile.links] : ['']
    selectedGenres.value = new Set(profile.favorite_genres)
    discoverable.value = profile.discoverable
    presenceVisibility.value = profile.presence_visibility
    hasSigningKey.value = loadSigningKey(profile.identity_id) !== null
    if (hasSigningKey.value) {
      await refreshDevicesAndPendingGrants()
    }
    await refreshPasskeys()
    await refreshBlockedUsers()
    await Promise.all([
      refreshGuardianSettings(),
      refreshMyRecoveryStatus(),
      refreshGuardianRequests(),
      refreshGuardianOf(),
    ])
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    loading.value = false
  }
  pollHandle = setInterval(() => {
    if (hasSigningKey.value) refreshDevicesAndPendingGrants()
    if (pendingRequest.value) pollMyGrant()
    // Social recovery (#201) is exactly the kind of security-relevant state
    // that must stay visible without a manual refresh — an in-progress
    // recovery against this identity, and any friend's request waiting on
    // this identity's approval, are both polled the same cadence as the
    // device-grant flow above.
    refreshMyRecoveryStatus()
    refreshGuardianRequests()
    refreshGuardianOf()
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
type ProfileField =
  | 'display_name'
  | 'avatar_url'
  | 'bio'
  | 'pronouns'
  | 'banner_url'
  | 'status'
  | 'timezone'
  | 'theme_color'
  | 'location'
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
    bio.value = profile.bio ?? ''
    pronouns.value = profile.pronouns ?? ''
    bannerUrl.value = profile.banner_url ?? ''
    status.value = profile.status ?? ''
    timezone.value = profile.timezone ?? ''
    themeColor.value = profile.theme_color ?? ''
    location.value = profile.location ?? ''
  } catch (e) {
    fieldErrors.value = {
      ...fieldErrors.value,
      [field]: e instanceof Error ? e.message : 'Something went wrong.',
    }
  } finally {
    savingField.value = ''
  }
}

// Issue #451: timezone and theme_color moved off the generic
// AvalonEditableField (read-then-click-to-edit) shape — a searchable
// dropdown and a color picker both want to be their own always-visible
// control, not text pretending to be editable-in-place. Both still save
// through `saveProfileField` above, just from an explicit Save button
// instead of AvalonEditableField's own commit-on-blur.
const timezoneOptions = listIanaTimezones()

// favorite_genres saves as one explicit action (not per-field like the
// text fields above) since it's a multi-select list, not a single value —
// same "toggle checkboxes, then press Save" shape the recovery-guardians
// picker below already uses.
function onToggleGenre(genre: Genre) {
  const next = new Set(selectedGenres.value)
  if (next.has(genre)) {
    next.delete(genre)
  } else if (next.size < MAX_FAVORITE_GENRES) {
    next.add(genre)
  }
  selectedGenres.value = next
}

async function onSaveGenres() {
  if (!session.token) return
  genresError.value = ''
  savingGenres.value = true
  try {
    const profile = await api.updateProfile(session.token, {
      favorite_genres: [...selectedGenres.value],
    })
    selectedGenres.value = new Set(profile.favorite_genres)
  } catch (e) {
    genresError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingGenres.value = false
  }
}

// `main_guild` saves as its own explicit action, same shape as
// onSaveGenres — a select-from-a-list field, not a free-text one, so it
// doesn't go through AvalonEditableField's text-input pattern.
const effectiveMainGuildName = computed(
  () => myGuilds.value.find((g) => g.id === effectiveMainGuild.value)?.name ?? null,
)

async function onSaveMainGuild() {
  if (!session.token) return
  mainGuildError.value = ''
  savingMainGuild.value = true
  try {
    const profile = await api.updateProfile(session.token, { main_guild: mainGuild.value })
    mainGuild.value = profile.main_guild ?? ''
    effectiveMainGuild.value = profile.effective_main_guild ?? ''
  } catch (e) {
    mainGuildError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingMainGuild.value = false
  }
}

function onLinkInput(index: number, value: string) {
  const next = [...links.value]
  next[index] = value
  links.value = next
}

function onAddLinkSlot() {
  if (links.value.length < MAX_LINKS) links.value = [...links.value, '']
}

async function onSaveLinks() {
  if (!session.token) return
  linksError.value = ''
  savingLinks.value = true
  try {
    const trimmed = links.value.map((l) => l.trim()).filter((l) => l.length > 0)
    const profile = await api.updateProfile(session.token, { links: trimmed })
    links.value = profile.links?.length ? [...profile.links] : ['']
  } catch (e) {
    linksError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingLinks.value = false
  }
}

// Flips the toggle and reads back the server's own value into `discoverable`
// rather than assuming the request succeeded as sent — same "trust the
// response, not the optimistic click" posture `saveProfileField` uses.
async function onToggleDiscoverable() {
  if (!session.token) return
  discoverableError.value = ''
  savingDiscoverable.value = true
  try {
    const profile = await api.updateProfile(session.token, { discoverable: !discoverable.value })
    discoverable.value = profile.discoverable
  } catch (e) {
    discoverableError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingDiscoverable.value = false
  }
}

const showRecovery = ref(false)
const recoveryPhrase = ref('')
const recovering = ref(false)
const recoveryError = ref('')

async function onRecoverSigningKey() {
  recoveryError.value = ''
  recovering.value = true
  try {
    recoverSigningKey(identityId.value, recoveryPhrase.value.trim())
    hasSigningKey.value = true
    showRecovery.value = false
    recoveryPhrase.value = ''
    // Issue #525: this device just gained a local signing key mid-session
    // — refresh the session store's cached signing_key_id so
    // reconnect-across-nodes becomes available without a fresh login.
    if (session.token) {
      const secretKey = loadSigningKey(identityId.value)
      session.setSigningKeyId(secretKey ? await findMySigningKeyId(session.token, secretKey) : null)
    }
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
      // Issue #525: same reconnect-signing_key_id refresh as
      // onRecoverSigningKey above — this device just gained its key via
      // approval instead of a recovery phrase.
      if (session.token) {
        const secretKey = loadSigningKey(identityId.value)
        session.setSigningKeyId(
          secretKey ? await findMySigningKeyId(session.token, secretKey) : null,
        )
      }
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

// #199: resurfaces for as long as the identity has exactly one registered
// passkey — checked from the live `GET /me/passkeys` count on every
// refresh, not a one-time dismissible notice. Disappears the moment a
// second passkey is registered, without a page reload. `passkeys.value`
// can only ever be a real array here — `refreshPasskeys` below refuses to
// assign anything else — so this never mistakes an unexpected/malformed
// response for "confirmed zero passkeys, no warning needed"; the
// `!Array.isArray` branch is defense in depth against exactly that
// silent-hide failure mode, not a path that should ever actually trigger.
const showSinglePasskeyWarning = computed(() => {
  if (!Array.isArray(passkeys.value)) return true
  return shouldShowSinglePasskeyWarning(passkeys.value.length)
})

async function refreshPasskeys() {
  if (!session.token) return
  try {
    const result = await listPasskeys(session.token)
    if (!Array.isArray(result)) {
      // A 200 with an unexpected body shape is still a failure worth
      // surfacing — throwing here routes it through the same catch below,
      // which leaves the previous known-good `passkeys.value` in place
      // (never silently replaced with something that isn't a real list)
      // and reports the error instead of pretending nothing happened.
      throw new Error('Unexpected response fetching passkeys.')
    }
    passkeys.value = result
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

// Social recovery (#201) — guardian configuration. Every guardian must be
// a current friend (crates/server/src/recovery.rs enforces this
// server-side too; the checkbox list below only ever offers friends as
// candidates, so there's no client path that could even attempt an
// invalid guardian).
const friends = ref<Friend[]>([])
const selectedGuardianIds = ref<Set<string>>(new Set())
const threshold = ref(1)
const savingGuardians = ref(false)
const guardiansError = ref('')
const guardiansUpdatedAt = ref<string | null>(null)

async function refreshGuardianSettings() {
  if (!session.token || !identityId.value) return
  try {
    const [friendList, settings] = await Promise.all([
      listFriendsWithPresence(session.token, identityId.value),
      getGuardians(session.token),
    ])
    friends.value = friendList
    selectedGuardianIds.value = new Set(settings.guardian_ids)
    threshold.value = settings.threshold > 0 ? settings.threshold : 1
    guardiansUpdatedAt.value = settings.updated_at
  } catch (e) {
    guardiansError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

function onToggleGuardian(friendId: string) {
  const next = new Set(selectedGuardianIds.value)
  if (next.has(friendId)) {
    next.delete(friendId)
  } else {
    next.add(friendId)
  }
  selectedGuardianIds.value = next
}

// Clamped client-side purely for a responsive slider/stepper feel — the
// server is the actual source of truth for "1..=guardian count"
// (recovery::validate_guardian_settings) and rejects anything outside
// that range regardless of what this does.
const clampedThreshold = computed({
  get: () => threshold.value,
  set: (value: number) => {
    const count = selectedGuardianIds.value.size || 1
    threshold.value = Math.min(Math.max(1, value), count)
  },
})

async function onSaveGuardians() {
  if (!session.token) return
  guardiansError.value = ''
  savingGuardians.value = true
  try {
    const settings = await setGuardians(
      session.token,
      [...selectedGuardianIds.value],
      threshold.value,
    )
    selectedGuardianIds.value = new Set(settings.guardian_ids)
    threshold.value = settings.threshold
    guardiansUpdatedAt.value = settings.updated_at
  } catch (e) {
    guardiansError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    savingGuardians.value = false
  }
}

// This identity's own in-progress recovery, if any — the "visible to the
// owner through every channel that still works for them" invariant, for
// the case this device/session still works. `GET /identities/:id/recovery/status`
// is also public (no session needed at all), for the case it doesn't.
const myRecoveryStatus = ref<RecoveryRequestResponse | null>(null)
const cancellingMyRecovery = ref(false)
const cancelMyRecoveryError = ref('')

async function refreshMyRecoveryStatus() {
  if (!session.token) return
  try {
    myRecoveryStatus.value = await getMyRecoveryStatus(session.token)
  } catch {
    // Non-fatal — this is a supplementary notice, not the page's primary
    // content; a failed poll just leaves the previous known state in
    // place rather than surfacing a page-level error.
  }
}

async function onCancelMyRecovery() {
  if (!session.token || !myRecoveryStatus.value) return
  cancelMyRecoveryError.value = ''
  cancellingMyRecovery.value = true
  try {
    myRecoveryStatus.value = await cancelRecoveryRequest(
      session.token,
      myRecoveryStatus.value.id,
      "This wasn't me",
    )
  } catch (e) {
    cancelMyRecoveryError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    cancellingMyRecovery.value = false
  }
}

// Recovery attempts against *other* identities where this identity is
// currently a guardian — the approval UI.
const guardianRequests = ref<GuardianRequestSummary[]>([])
const guardianProfileNames = ref<Record<string, string>>({})
const actingOnRequestId = ref('')
const guardianRequestError = ref('')

async function refreshGuardianRequests() {
  if (!session.token) return
  try {
    const summaries = await getGuardianRequests(session.token)
    // A malformed/empty response is treated as "nothing pending" rather
    // than assigned as-is — this is a polled, supplementary list (unlike
    // the passkeys list above, which throws on a bad shape), so failing
    // quietly here is the right default, but it must never leave
    // `guardianRequests.value` as anything other than a real array.
    if (!Array.isArray(summaries)) {
      guardianRequests.value = []
      return
    }
    guardianRequests.value = summaries
    const ids = [...new Set(summaries.map((s) => s.request.identity_id))].filter(
      (id) => !(id in guardianProfileNames.value),
    )
    if (ids.length > 0) {
      const profiles = await api.getProfiles(session.token, ids)
      const names = { ...guardianProfileNames.value }
      for (const profile of profiles) {
        names[profile.identity_id] = profile.display_name
      }
      guardianProfileNames.value = names
    }
  } catch {
    // Same non-fatal treatment as refreshMyRecoveryStatus above.
  }
}

async function onApproveGuardianRequest(requestId: string) {
  if (!session.token) return
  guardianRequestError.value = ''
  actingOnRequestId.value = requestId
  try {
    await approveRecoveryRequest(session.token, requestId)
    await refreshGuardianRequests()
  } catch (e) {
    guardianRequestError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    actingOnRequestId.value = ''
  }
}

async function onCancelGuardianRequest(requestId: string) {
  if (!session.token) return
  guardianRequestError.value = ''
  actingOnRequestId.value = requestId
  try {
    await cancelRecoveryRequest(session.token, requestId, 'I do not believe this is legitimate')
    await refreshGuardianRequests()
  } catch (e) {
    guardianRequestError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    actingOnRequestId.value = ''
  }
}

// Issue #443: identities that currently name this identity as one of
// *their* guardians — visibility into a responsibility the owner-side
// config (above) can otherwise hand out without the guardian ever knowing.
const guardianOf = ref<GuardianOfSummary[]>([])
const guardianOfError = ref('')
const resigningFrom = ref('')

async function refreshGuardianOf() {
  if (!session.token) return
  try {
    const summaries = await getGuardianOf(session.token)
    // Same "never leave this as anything but a real array" guard as
    // refreshGuardianRequests above — this is a polled, supplementary list.
    guardianOf.value = Array.isArray(summaries) ? summaries : []
    // Issue #466: actually rendering this section is what "reads" a new
    // guardian designation — mirrors markConversationSeen/
    // onSelectAnnouncement's own "visiting the real feature clears it"
    // idiom, not a poll succeeding in the background.
    markGuardianOfSeen(guardianOf.value.map((g) => g.identity_id))
  } catch {
    // Same non-fatal treatment as refreshGuardianRequests above.
  }
}

async function onResignGuardian(identityId: string) {
  if (!session.token) return
  guardianOfError.value = ''
  resigningFrom.value = identityId
  try {
    await resignAsGuardian(session.token, identityId)
    await refreshGuardianOf()
  } catch (e) {
    guardianOfError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    resigningFrom.value = ''
  }
}
</script>

<template>
  <div v-if="!loading" :class="page.page">
    <header :class="styles.hero">
      <AvalonAvatar :src="avatarUrl || null" :name="displayName" size="xl" />
      <div :class="styles.heroText">
        <h1 :class="page.title">{{ displayName }}</h1>
        <p :class="styles.identityId">{{ identityId }}</p>
      </div>
      <div :class="styles.heroActions">
        <AvalonButton label="Log out" variant="secondary" @click="onLogout" />
      </div>
    </header>
    <p v-if="error" :class="page.error">{{ error }}</p>

    <template v-if="myRecoveryStatus">
      <AvalonWarningBanner
        tone="danger"
        title="A recovery attempt is in progress against your identity"
        :message="`Status: ${myRecoveryStatus.status}. If this isn't you, cancel it now — a recovery only completes if it goes unvetoed until its time-delay elapses.`"
      />
      <div :class="styles.actions">
        <AvalonButton
          :label="cancellingMyRecovery ? 'Cancelling…' : 'This was not me — cancel it'"
          variant="danger"
          :disabled="cancellingMyRecovery"
          @click="onCancelMyRecovery"
        />
      </div>
      <p v-if="cancelMyRecoveryError" :class="page.error">{{ cancelMyRecoveryError }}</p>
    </template>

    <AvalonCard title="User search" :class="styles.discoverabilityCard">
      <div :class="styles.discoverabilityRow">
        <div :class="styles.discoverabilityText">
          <p :class="styles.discoverabilityStatus">
            <span
              :class="[styles.discoverabilityDot, discoverable ? styles.discoverabilityDotOn : '']"
            />
            {{ discoverable ? 'You are currently publicly searchable.' : 'You are not publicly searchable.' }}
          </p>
          <p :class="styles.listDetail">
            Turning this on lets any user find you by name in search. Off by
            default — turning it off removes you from search immediately.
          </p>
        </div>
        <AvalonButton
          :label="savingDiscoverable ? 'Saving…' : discoverable ? 'Turn off' : 'Turn on'"
          :variant="discoverable ? 'secondary' : 'primary'"
          :disabled="savingDiscoverable"
          @click="onToggleDiscoverable"
        />
      </div>
      <p v-if="discoverableError" :class="page.error">{{ discoverableError }}</p>
    </AvalonCard>

    <AvalonCard
      title="Online status visibility"
      subtitle="Who can see whether you're online, away, or offline."
    >
      <select
        v-model="presenceVisibility"
        :class="styles.linkInput"
        aria-label="Online status visibility"
      >
        <option v-for="opt in VISIBILITY_OPTIONS" :key="opt.value" :value="opt.value">
          {{ opt.label }}
        </option>
      </select>
      <div :class="styles.actions">
        <AvalonButton
          :label="savingPresenceVisibility ? 'Saving…' : 'Save'"
          variant="primary"
          :disabled="savingPresenceVisibility"
          @click="onSavePresenceVisibility"
        />
      </div>
      <p v-if="presenceVisibilityError" :class="page.error">{{ presenceVisibilityError }}</p>
    </AvalonCard>

    <AvalonCard
      title="Blocked users"
      subtitle="Blocking doesn't tell the other person, and doesn't remove an existing friendship on its own."
    >
      <p v-if="blockedUsers.length === 0" :class="page.empty">You haven't blocked anyone.</p>
      <ul v-else :class="styles.list">
        <li v-for="blocked in blockedUsers" :key="blocked.identityId" :class="styles.guardianRow">
          <span>{{ blocked.displayName ?? blocked.identityId }}</span>
          <AvalonButton
            :label="unblockingId === blocked.identityId ? 'Unblocking…' : 'Unblock'"
            variant="secondary"
            :disabled="unblockingId === blocked.identityId"
            @click="onUnblock(blocked.identityId)"
          />
        </li>
      </ul>
      <div :class="styles.stack">
        <input
          v-model="blockByIdInput"
          type="text"
          placeholder="identity id or display name"
          :class="styles.linkInput"
        />
        <AvalonButton
          :label="blockingById ? 'Blocking…' : 'Block'"
          variant="danger"
          :disabled="blockingById || !blockByIdInput.trim()"
          @click="onBlockById"
        />
      </div>
      <p v-if="blockedUsersError" :class="page.error">{{ blockedUsersError }}</p>
      <p v-if="blockByIdError" :class="page.error">{{ blockByIdError }}</p>
    </AvalonCard>

    <div :class="page.grid">
      <div :class="page.mainColumn">
        <AvalonCard title="Profile" subtitle="How other users see you.">
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
            <AvalonEditableField
              label="Bio"
              :value="bio"
              empty-text="No bio"
              placeholder="Say something about yourself…"
              :saving="savingField === 'bio'"
              :error="fieldErrors.bio"
              @save="saveProfileField('bio', $event)"
            />
            <AvalonEditableField
              label="Pronouns"
              :value="pronouns"
              empty-text="No pronouns set"
              placeholder="they/them"
              :saving="savingField === 'pronouns'"
              :error="fieldErrors.pronouns"
              @save="saveProfileField('pronouns', $event)"
            />
            <AvalonEditableField
              label="Banner URL"
              :value="bannerUrl"
              empty-text="No banner"
              placeholder="https://…"
              :saving="savingField === 'banner_url'"
              :error="fieldErrors.banner_url"
              @save="saveProfileField('banner_url', $event)"
            />
            <AvalonEditableField
              label="Status"
              :value="status"
              empty-text="No status set"
              placeholder="What are you up to?"
              :saving="savingField === 'status'"
              :error="fieldErrors.status"
              @save="saveProfileField('status', $event)"
            />
            <div :class="styles.fieldWithAction">
              <label :class="styles.fieldLabel" for="profile-timezone">Timezone</label>
              <input
                id="profile-timezone"
                v-model="timezone"
                :class="styles.timezoneInput"
                type="text"
                list="profile-timezone-options"
                placeholder="America/New_York"
              />
              <datalist id="profile-timezone-options">
                <option v-for="tz in timezoneOptions" :key="tz" :value="tz" />
              </datalist>
              <p v-if="fieldErrors.timezone" :class="page.error">{{ fieldErrors.timezone }}</p>
              <AvalonButton
                :label="savingField === 'timezone' ? 'Saving…' : 'Save timezone'"
                variant="secondary"
                :disabled="savingField === 'timezone'"
                @click="saveProfileField('timezone', timezone)"
              />
            </div>
            <div :class="styles.fieldWithAction">
              <AvalonColorPicker
                v-model="themeColor"
                label="Theme color"
                :error="fieldErrors.theme_color"
              />
              <AvalonButton
                :label="savingField === 'theme_color' ? 'Saving…' : 'Save theme color'"
                variant="secondary"
                :disabled="savingField === 'theme_color'"
                @click="saveProfileField('theme_color', themeColor)"
              />
            </div>
            <AvalonEditableField
              label="Location"
              :value="location"
              empty-text="No location set"
              placeholder="Pacific Northwest"
              :saving="savingField === 'location'"
              :error="fieldErrors.location"
              @save="saveProfileField('location', $event)"
            />
          </div>

          <div :class="styles.stack">
            <h3 :class="styles.subheading">Main guild</h3>
            <p :class="styles.listDetail">
              Pick one of your own guilds to build around, or leave unset to default to the guild
              you joined earliest<span v-if="effectiveMainGuildName"> ({{ effectiveMainGuildName }})</span>.
            </p>
            <p v-if="mainGuildError" :class="page.error">{{ mainGuildError }}</p>
            <select v-model="mainGuild" :class="styles.linkInput" aria-label="Main guild">
              <option value="">No main guild set</option>
              <option v-for="guild in myGuilds" :key="guild.id" :value="guild.id">
                {{ guild.name }}
              </option>
            </select>
            <div :class="styles.actions">
              <AvalonButton
                :label="savingMainGuild ? 'Saving…' : 'Save main guild'"
                variant="primary"
                :disabled="savingMainGuild"
                @click="onSaveMainGuild"
              />
            </div>
          </div>

          <div :class="styles.stack">
            <h3 :class="styles.subheading">Links</h3>
            <p :class="styles.listDetail">Up to {{ MAX_LINKS }} URLs (http/https).</p>
            <p v-if="linksError" :class="page.error">{{ linksError }}</p>
            <ul :class="styles.list">
              <li v-for="(link, index) in links" :key="index" :class="styles.guardianRow">
                <input
                  type="url"
                  :value="link"
                  placeholder="https://…"
                  :class="styles.linkInput"
                  @input="onLinkInput(index, ($event.target as HTMLInputElement).value)"
                />
              </li>
            </ul>
            <div :class="styles.actions">
              <AvalonButton
                v-if="links.length < MAX_LINKS"
                label="Add another link"
                variant="secondary"
                @click="onAddLinkSlot"
              />
              <AvalonButton
                :label="savingLinks ? 'Saving…' : 'Save links'"
                variant="primary"
                :disabled="savingLinks"
                @click="onSaveLinks"
              />
            </div>
          </div>

          <div :class="styles.stack">
            <h3 :class="styles.subheading">Favorite genres</h3>
            <p :class="styles.listDetail">Pick up to {{ MAX_FAVORITE_GENRES }}.</p>
            <p v-if="genresError" :class="page.error">{{ genresError }}</p>
            <ul :class="styles.list">
              <li v-for="genre in GENRE_OPTIONS" :key="genre.value" :class="styles.guardianRow">
                <input
                  type="checkbox"
                  :id="`genre-${genre.value}`"
                  :checked="selectedGenres.has(genre.value)"
                  :disabled="!selectedGenres.has(genre.value) && selectedGenres.size >= MAX_FAVORITE_GENRES"
                  @change="onToggleGenre(genre.value)"
                />
                <label :for="`genre-${genre.value}`">{{ genre.label }}</label>
              </li>
            </ul>
            <div :class="styles.actions">
              <AvalonButton
                :label="savingGenres ? 'Saving…' : 'Save favorite genres'"
                variant="primary"
                :disabled="savingGenres"
                @click="onSaveGenres"
              />
            </div>
          </div>
        </AvalonCard>

        <AvalonCard
          v-if="!hasSigningKey"
          title="Set up this device's signing key"
          subtitle="Separate from passkeys: this is the key this identity signs friend requests, guild actions, and other events with, not the key you log in with."
        >
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
          <AvalonWarningBanner
            v-if="showSinglePasskeyWarning"
            tone="danger"
            title="You have only one passkey"
            message="If you lose this device, or it stops working, you'll permanently lose this identity and everything durable it carries — friends, guild history, and achievements. Register a second passkey from another device now, before that happens."
          />
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

        <AvalonCard
          title="Recovery guardians"
          subtitle="Trusted friends who can jointly authorize recovering this identity if you ever lose every passkey at once. Requires a threshold (M-of-N) so no single guardian can act alone, plus a mandatory public delay before it takes effect."
        >
          <p v-if="guardiansError" :class="page.error">{{ guardiansError }}</p>
          <p v-if="friends.length === 0" :class="page.empty">
            You need at least one friend before you can designate a guardian.
          </p>
          <ul v-else :class="styles.list">
            <li v-for="friend in friends" :key="friend.identityId" :class="styles.guardianRow">
              <input
                type="checkbox"
                :id="`guardian-${friend.identityId}`"
                :checked="selectedGuardianIds.has(friend.identityId)"
                @change="onToggleGuardian(friend.identityId)"
              />
              <label :for="`guardian-${friend.identityId}`">
                {{ friend.displayName ?? friend.identityId }}
              </label>
            </li>
          </ul>
          <div v-if="selectedGuardianIds.size > 0" :class="styles.thresholdRow">
            <label :for="'guardian-threshold'">Require at least</label>
            <input
              id="guardian-threshold"
              type="number"
              min="1"
              :max="selectedGuardianIds.size"
              v-model.number="clampedThreshold"
              :class="styles.thresholdInput"
            />
            <span>of {{ selectedGuardianIds.size }} guardian{{ selectedGuardianIds.size === 1 ? '' : 's' }} to approve</span>
          </div>
          <p v-if="guardiansUpdatedAt" :class="styles.listDetail">
            Last updated {{ guardiansUpdatedAt }}
          </p>
          <div :class="styles.actions">
            <AvalonButton
              :label="savingGuardians ? 'Saving…' : 'Save guardians'"
              variant="primary"
              :disabled="savingGuardians || selectedGuardianIds.size === 0"
              @click="onSaveGuardians"
            />
          </div>
        </AvalonCard>

        <AvalonCard
          v-if="guardianRequests.length > 0"
          title="Recovery requests to approve"
          subtitle="A friend who made you a guardian has a recovery attempt in progress. Approve only if you're confident it's really them."
        >
          <p v-if="guardianRequestError" :class="page.error">{{ guardianRequestError }}</p>
          <ul :class="styles.list">
            <li
              v-for="summary in guardianRequests"
              :key="summary.request.id"
              :class="styles.device"
            >
              <span :class="styles.listLabel">
                {{ guardianProfileNames[summary.request.identity_id] ?? summary.request.identity_id }}
              </span>
              <span :class="styles.listDetail">
                {{ summary.request.approvals_count }} of {{ summary.request.threshold }} approvals ·
                status: {{ summary.request.status }}
                <template v-if="summary.request.delay_ends_at">
                  · delay ends {{ summary.request.delay_ends_at }}
                </template>
              </span>
              <div :class="styles.deviceActions">
                <AvalonButton
                  v-if="!summary.already_approved"
                  :label="actingOnRequestId === summary.request.id ? 'Approving…' : 'Approve'"
                  variant="primary"
                  :disabled="actingOnRequestId === summary.request.id"
                  @click="onApproveGuardianRequest(summary.request.id)"
                />
                <span v-else :class="styles.listDetail">You approved this request.</span>
                <AvalonButton
                  :label="actingOnRequestId === summary.request.id ? 'Working…' : 'This looks malicious — cancel'"
                  variant="danger"
                  :disabled="actingOnRequestId === summary.request.id"
                  @click="onCancelGuardianRequest(summary.request.id)"
                />
              </div>
            </li>
          </ul>
        </AvalonCard>

        <AvalonCard
          v-if="guardianOf.length > 0"
          title="You're a recovery guardian for"
          subtitle="These people have named you as a trusted guardian — your approval counts toward the threshold that can recover their identity. You can stop being a guardian at any time, without their cooperation."
        >
          <p v-if="guardianOfError" :class="page.error">{{ guardianOfError }}</p>
          <ul :class="styles.list">
            <li v-for="entry in guardianOf" :key="entry.identity_id" :class="styles.listRow">
              <span :class="styles.listText">
                <span :class="styles.listLabel">{{ entry.display_name }}</span>
                <span :class="styles.listDetail">guardian since {{ entry.added_at }}</span>
              </span>
              <AvalonButton
                :label="resigningFrom === entry.identity_id ? 'Removing…' : 'Stop being a guardian'"
                variant="danger"
                :disabled="resigningFrom === entry.identity_id"
                @click="onResignGuardian(entry.identity_id)"
              />
            </li>
          </ul>
        </AvalonCard>

        <AvalonCard
          v-if="hasSigningKey && pendingGrants.length > 0"
          title="Signing devices waiting for your approval"
          subtitle="Separate from passkeys above — these hold the key this identity uses to author friend requests, guild actions, and other events, not to log in."
        >
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
          title="Signing devices"
          subtitle="Separate from passkeys above: these hold the key this identity signs friend requests, guild actions, and other events with, not the key you log in with. To add another device, log into this identity there — with no signing key yet, it'll offer to request access, and the request will show up here for you to approve."
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
