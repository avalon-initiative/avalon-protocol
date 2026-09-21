// Per-integrator profile page (issue #270, first slice of #90): GET
// /integrations/{slug} (#293's canonical alias for GET /integrations/{slug},
// public fields) + GET /integrations/{slug}/registry (#261's five class-labeled
// metrics — not renamed by #293) + GET /integrations/{slug}/keys (issuer key
// history), fetched together since the profile page always needs all
// three. All three reads are public/unauthenticated — no session token.
import { computed, ref, watch } from 'vue'
import type { Ref } from 'vue'
import { getIntegrator, getIntegratorRegistry, listIssuerKeys } from '@avalon/sdk'
import type { Integrator, IntegratorRegistry, IssuerKey } from '@avalon/sdk'
import { listRegistryMetrics } from '../api/integrations'
import { getServerUrl } from '../api/serverUrl'

export function useIntegrationProfile(slug: Ref<string>) {
  const integrator = ref<Integrator | null>(null)
  const registry = ref<IntegratorRegistry | null>(null)
  const metrics = computed(() => (registry.value ? listRegistryMetrics(registry.value) : []))
  const issuerKeys = ref<IssuerKey[]>([])
  const loading = ref(true)
  const error = ref('')

  async function load() {
    loading.value = true
    error.value = ''
    try {
      const serverUrl = getServerUrl()
      const [integratorResponse, registryResponse, keysResponse] = await Promise.all([
        getIntegrator(serverUrl, slug.value),
        getIntegratorRegistry(serverUrl, slug.value),
        listIssuerKeys(serverUrl, slug.value),
      ])
      integrator.value = integratorResponse
      registry.value = registryResponse
      issuerKeys.value = Array.isArray(keysResponse) ? keysResponse : []
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      loading.value = false
    }
  }

  watch(slug, load, { immediate: true })

  return { integrator, registry, metrics, issuerKeys, loading, error, load }
}
