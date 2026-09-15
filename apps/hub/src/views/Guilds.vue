<script setup lang="ts">
// "My guilds" landing page (issue #24): every guild the caller belongs to
// (GET /me/guilds), plus a header-button "create guild" form in a modal
// (issue #281).
import { computed, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import {
  AvalonButton,
  AvalonCard,
  AvalonFilterBar,
  AvalonForm,
  AvalonGuildCard,
  AvalonModal,
  AvalonTextField,
} from '@avalon/ui'
import * as api from '../api/client'
import { canApplyToJoinGuild, filterGuildsByNameOrTag } from '../api/guilds'
import { useDiscoverGuilds } from '../composables/useDiscoverGuilds'
import { useMyGuildInvites } from '../composables/useMyGuildInvites'
import { useMyGuilds } from '../composables/useMyGuilds'
import { useSessionStore } from '../stores/session'
import local from '../styles/Guilds.module.scss'
import styles from '../styles/page.module.scss'

const router = useRouter()
const session = useSessionStore()
const { guilds, loading, error, refresh } = useMyGuilds()

// Issue #442: pending invites the caller has received, actionable right
// from the landing page.
const {
  invites: pendingInvites,
  inviterNames,
  refresh: refreshInvites,
} = useMyGuildInvites()
const respondingToInvite = ref<string | null>(null)
const inviteError = ref('')

async function onAcceptInvite(invite: (typeof pendingInvites.value)[number]) {
  if (!session.token) return
  inviteError.value = ''
  respondingToInvite.value = invite.id
  try {
    await api.acceptGuildInvite(session.token, invite.guild_id, invite.id)
    await Promise.all([refreshInvites(), refresh()])
    router.push({ name: 'guild', params: { id: invite.guild_id } })
  } catch (e) {
    inviteError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    respondingToInvite.value = null
  }
}

async function onDeclineInvite(invite: (typeof pendingInvites.value)[number]) {
  if (!session.token) return
  inviteError.value = ''
  respondingToInvite.value = invite.id
  try {
    await api.declineGuildInvite(session.token, invite.guild_id, invite.id)
    await refreshInvites()
  } catch (e) {
    inviteError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    respondingToInvite.value = null
  }
}

const guildQuery = ref('')
const visibleGuilds = computed(() => filterGuildsByNameOrTag(guilds.value, guildQuery.value))

// Issue #154: a "Discover" tab alongside "My guilds" — browse/search
// recruiting guilds, no membership required. Its own composable owns
// fetching/filtering; this view just wires the controls to it.
const activeTab = ref<'mine' | 'discover'>('mine')
const discover = useDiscoverGuilds()
let discoverLoaded = false
watch(activeTab, (tab) => {
  if (tab === 'discover' && !discoverLoaded) {
    discoverLoaded = true
    discover.refresh()
  }
})

const showCreateGuild = ref(false)
const createName = ref('')
const createTag = ref('')
const createDescription = ref('')
const creating = ref(false)
const createError = ref('')

function cancelCreateGuild() {
  showCreateGuild.value = false
  createName.value = ''
  createTag.value = ''
  createDescription.value = ''
  createError.value = ''
}

async function onCreateGuild() {
  if (!session.token) return
  createError.value = ''
  creating.value = true
  try {
    const guild = await api.createGuild(session.token, {
      name: createName.value.trim(),
      tag: createTag.value.trim(),
      description: createDescription.value.trim(),
    })
    cancelCreateGuild()
    await refresh()
    router.push({ name: 'guild', params: { id: guild.id } })
  } catch (e) {
    createError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    creating.value = false
  }
}

function openGuild(guildId: string) {
  router.push({ name: 'guild', params: { id: guildId } })
}

// Issue #242: "Apply to join" on a Discover card. `myGuildIds` comes from
// the "My guilds" tab's own fetch (`guilds` above) — no separate membership
// lookup. `appliedGuildIds` tracks guilds the caller has successfully
// applied to this session, so the button flips to a confirmation instead of
// re-submitting (harmless either way — the server treats a duplicate apply
// as idempotent — but this reads better than a button that looks like it
// does nothing on a second click).
const myGuildIds = computed(() => guilds.value.map((g) => g.id))
const appliedGuildIds = ref(new Set<string>())
const applyingTo = ref<string | null>(null)
const applyError = ref('')

async function onApplyToJoin(guildId: string) {
  if (!session.token) return
  applyError.value = ''
  applyingTo.value = guildId
  try {
    await api.createJoinRequest(session.token, guildId, {})
    appliedGuildIds.value = new Set(appliedGuildIds.value).add(guildId)
  } catch (e) {
    applyError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    applyingTo.value = null
  }
}
</script>

<template>
  <div v-if="!loading" :class="styles.page">
    <header :class="styles.pageHeader">
      <div :class="local.headerRow">
        <h1 :class="styles.title">Guilds</h1>
        <AvalonButton label="Create a guild" variant="primary" @click="showCreateGuild = true" />
      </div>
      <p :class="styles.subtitle">Communities you belong to, wherever their members are playing.</p>
    </header>
    <p v-if="error" :class="styles.error">{{ error }}</p>

    <AvalonCard v-if="pendingInvites.length > 0" title="Guild invites" :class="local.invitesCard">
      <p v-if="inviteError" :class="styles.error">{{ inviteError }}</p>
      <div v-for="invite in pendingInvites" :key="invite.id" :class="local.inviteRow">
        <span>
          <strong>{{ inviterNames[invite.from] ?? invite.from }}</strong>
          invited you to <strong>{{ invite.guild_name }}</strong>
        </span>
        <div :class="local.inviteActions">
          <AvalonButton
            label="Accept"
            variant="primary"
            :disabled="respondingToInvite === invite.id"
            @click="onAcceptInvite(invite)"
          />
          <AvalonButton
            label="Decline"
            variant="secondary"
            :disabled="respondingToInvite === invite.id"
            @click="onDeclineInvite(invite)"
          />
        </div>
      </div>
    </AvalonCard>

    <div :class="local.tabs">
      <button
        type="button"
        :class="[local.tab, activeTab === 'mine' && local.tabActive]"
        @click="activeTab = 'mine'"
      >
        My guilds
      </button>
      <button
        type="button"
        :class="[local.tab, activeTab === 'discover' && local.tabActive]"
        @click="activeTab = 'discover'"
      >
        Discover
      </button>
    </div>

    <div v-if="activeTab === 'mine'" :class="styles.mainColumn">
      <AvalonCard :title="`My guilds (${guilds.length})`">
        <p v-if="guilds.length === 0" :class="styles.empty">
          You're not in any guilds yet — create one to get started.
        </p>
        <template v-else>
          <AvalonFilterBar
            label="Search by name or tag"
            placeholder="Ashen Vanguard"
            :query="guildQuery"
            @update:query="guildQuery = $event"
          />
          <p v-if="guildQuery && visibleGuilds.length === 0" :class="styles.empty">
            No guilds match "{{ guildQuery }}".
          </p>
        </template>
        <AvalonGuildCard
          v-for="guild in visibleGuilds"
          :key="guild.id"
          :name="guild.name"
          :tag="guild.tag"
          :description="guild.description"
          :member-count="guild.member_count"
          :icon-url="guild.icon ?? undefined"
          :banner-url="guild.banner ?? undefined"
          @select="openGuild(guild.id)"
        />
      </AvalonCard>
    </div>

    <div v-else :class="styles.mainColumn">
      <AvalonCard title="Discover guilds" subtitle="Browse and search guilds that are recruiting new members.">
        <div :class="local.discoverFilters">
          <AvalonFilterBar
            label="Search by name, tag, or description"
            placeholder="Ashen Vanguard"
            :query="discover.query.value"
            no-margin
            @update:query="discover.query.value = $event"
          />
          <AvalonTextField v-model="discover.tag.value" label="Tag" placeholder="ASHV" />
          <label :class="local.recruitingToggle">
            <input v-model="discover.recruitingOnly.value" type="checkbox" />
            Recruiting only
          </label>
        </div>
        <p v-if="discover.error.value" :class="styles.error">{{ discover.error.value }}</p>
        <p v-else-if="!discover.loading.value && discover.guilds.value.length === 0" :class="styles.empty">
          No guilds match your search.
        </p>
        <p v-if="applyError" :class="styles.error">{{ applyError }}</p>
        <div v-for="guild in discover.guilds.value" :key="guild.id" :class="local.discoverCard">
          <AvalonGuildCard
            :name="guild.name"
            :tag="guild.tag"
            :description="guild.description"
            :member-count="guild.member_count"
            :recruiting="guild.recruiting"
            :icon-url="guild.icon ?? undefined"
            :banner-url="guild.banner ?? undefined"
            @select="openGuild(guild.id)"
          />
          <AvalonButton
            v-if="appliedGuildIds.has(guild.id)"
            label="Applied"
            variant="secondary"
            disabled
          />
          <AvalonButton
            v-else-if="canApplyToJoinGuild(guild, myGuildIds)"
            :label="applyingTo === guild.id ? 'Applying…' : 'Apply to join'"
            variant="secondary"
            :disabled="applyingTo === guild.id"
            @click="onApplyToJoin(guild.id)"
          />
        </div>
        <AvalonButton
          v-if="discover.nextCursor.value"
          label="Load more"
          variant="secondary"
          @click="discover.loadMore()"
        />
      </AvalonCard>
    </div>

    <AvalonModal title="Create a guild" :open="showCreateGuild" @close="cancelCreateGuild">
      <AvalonForm
        submit-label="Create guild"
        :submitting="creating"
        :error="createError"
        @submit="onCreateGuild"
      >
        <AvalonTextField v-model="createName" label="Name" placeholder="Ashen Vanguard" />
        <AvalonTextField v-model="createTag" label="Tag (2-5 characters)" placeholder="ASHV" :maxlength="5" />
        <AvalonTextField
          v-model="createDescription"
          label="Description (optional)"
          placeholder="What's this guild about?"
        />
        <template #secondary-actions>
          <AvalonButton label="Cancel" variant="secondary" @click="cancelCreateGuild" />
        </template>
      </AvalonForm>
    </AvalonModal>
  </div>
</template>
