// Integrator-connect consent view for a given slug: loads the integrator's
// public registration info once (no polling needed — an integrator's declared
// capabilities don't change while a user is looking at the consent
// screen), and owns the checked-capabilities set the view submits.
import { ref, type Ref } from 'vue'
import { getIntegrator, type Integrator } from '@avalon/sdk'
import { getServerUrl } from '../api/serverUrl'

export function useIntegrationConsent(slug: Ref<string>) {
  const integrator = ref<Integrator | null>(null)
  const loading = ref(true)
  const error = ref('')

  // Unchecked by default (the ticket's own invariant) — starts empty
  // regardless of what the integrator requested.
  const checkedCapabilities = ref<Set<string>>(new Set())

  async function load() {
    loading.value = true
    error.value = ''
    try {
      integrator.value = await getIntegrator(getServerUrl(), slug.value)
      checkedCapabilities.value = new Set()
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      loading.value = false
    }
  }

  function setCapabilityChecked(capability: string, checked: boolean) {
    const next = new Set(checkedCapabilities.value)
    if (checked) {
      next.add(capability)
    } else {
      next.delete(capability)
    }
    checkedCapabilities.value = next
  }

  return { integrator, loading, error, checkedCapabilities, setCapabilityChecked, load }
}
