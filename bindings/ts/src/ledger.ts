// Public, unauthenticated ledger reads — free-standing
// functions, not session methods, since `GET /ledger/sth/latest` needs no
// session at all. See crates/server/src/settlement.rs::latest_sth.
import { request } from './http.js'
import type { SignedTreeHeadResponse } from './types.js'

/** `GET /ledger/sth/latest` — the network's current Signed Tree Head.
 * Verification of the returned signature/root hash is Hub's own logic
 * (`apps/hub/src/network/verifyNetwork.ts`), not this SDK's —
 * this just fetches the wire shape. */
export function getLatestSth(serverUrl: string): Promise<SignedTreeHeadResponse> {
  return request<SignedTreeHeadResponse>(serverUrl, '/ledger/sth/latest')
}
