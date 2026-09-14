// Per-integrator profile page (issue #270, first slice of #90): GET
// /integrations/{slug} (#293's canonical alias for GET /integrators/{slug},
// public fields) + GET /integrators/{slug}/registry (#261's five class-labeled
// metrics — not renamed by #293) + GET /integrators/{slug}/keys (issuer key
// history), fetched together since the profile page always needs all
// three. All three reads are public/unauthenticated — no session token.
import { computed, ref, watch } from 'vue'
import type { Ref } from 'vue'
import * as api from '../api/client'
import { listRegistryMetrics } from '../api/integrations'
import type { IntegratorResponse, IssuerKeyResponse } from '../api/types'

export function useIntegrationProfile(slug: Ref<string>) {
  const integrator = ref<IntegratorResponse | null>(null)
  const registry = ref<Awaited<ReturnType<typeof api.getIntegratorRegistry>> | null>(null)
  const metrics = computed(() => (registry.value ? listRegistryMetrics(registry.value) : []))
  const issuerKeys = ref<IssuerKeyResponse[]>([])
  const loading = ref(true)
  const error = ref('')

  async function load() {
    loading.value = true
    error.value = ''
    try {
      const [integratorResponse, registryResponse, keysResponse] = await Promise.all([
        api.getIntegratorPublic(slug.value),
        api.getIntegratorRegistry(slug.value),
        api.listIssuerKeys(slug.value),
      ])
      integrator.value = integratorResponse
      registry.value = registryResponse
      issuerKeys.value = keysResponse
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      loading.value = false
    }
  }

  watch(slug, load, { immediate: true })

  return { integrator, registry, metrics, issuerKeys, loading, error, load }
}
