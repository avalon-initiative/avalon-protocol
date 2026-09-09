// Issue #232: fetches the connected server's latest Signed Tree Head and
// checks it against the Hub's bundled trust-anchor list, so `HubShell.vue`
// can show — always visibly, never buried in settings — whether this
// session is actually talking to a pinned, verified Avalon network.
import { onMounted, ref } from 'vue'
import { getLatestSth } from '../api/client'
import { getBundledTrustAnchors, type TrustAnchorEntry } from '../network/trustAnchors'
import { evaluateNetworkTrust, type NetworkTrustStatus } from '../network/verifyNetwork'

export type NetworkTrustState =
  | { kind: 'loading' }
  // The STH request itself failed (network error, server down, non-2xx) —
  // distinct from a resolved-but-untrusted status below.
  | { kind: 'unreachable'; message: string }
  | NetworkTrustStatus

export function useNetworkTrust() {
  const state = ref<NetworkTrustState>({ kind: 'loading' })
  const knownNetworks = ref<TrustAnchorEntry[]>(getBundledTrustAnchors())

  async function refresh() {
    state.value = { kind: 'loading' }
    try {
      const sth = await getLatestSth()
      if (!sth || typeof sth.network_id !== 'string') {
        throw new Error('Server did not return a usable Signed Tree Head.')
      }
      state.value = evaluateNetworkTrust(knownNetworks.value, sth)
    } catch (e) {
      state.value = {
        kind: 'unreachable',
        message: e instanceof Error ? e.message : 'Could not reach the network.',
      }
    }
  }

  onMounted(refresh)

  return { state, knownNetworks, refresh }
}
