// Public, unauthenticated per-identity integrator-published data (issue
// #384/#465) — free-standing, same convention as ledger.ts's getLatestSth.
// See crates/server/src/integrator_data.rs.
import { request } from './http.js'

export interface VisibleIntegratorDataInstance {
  schema: string
  integratorId: string
  publishedAt: string
  fields: Record<string, unknown>
}
interface VisibleIntegratorDataInstanceWire {
  schema: string
  integrator_id: string
  published_at: string
  fields: Record<string, unknown>
}

/** `GET /identities/{id}/integrator-data` — every current instance a
 * connected integrator has published about this identity, already filtered
 * server-side to only the fields that integrator's schema currently makes
 * visible; no field-level filtering belongs on top of this client-side. */
export async function getIdentityIntegratorData(
  serverUrl: string,
  identityId: string,
): Promise<VisibleIntegratorDataInstance[]> {
  const w = await request<VisibleIntegratorDataInstanceWire[]>(serverUrl, `/identities/${identityId}/integrator-data`)
  if (!Array.isArray(w)) return w as unknown as VisibleIntegratorDataInstance[]
  return w.map((d) => ({ schema: d.schema, integratorId: d.integrator_id, publishedAt: d.published_at, fields: d.fields }))
}

interface LocationsResponseWire {
  locations: string[]
}

/** `GET /identities/{id}/locations` — the network node base URLs this
 * identity's data can currently be located at. Public, unauthenticated. */
export async function getLocations(serverUrl: string, identityId: string): Promise<string[]> {
  const w = await request<LocationsResponseWire>(serverUrl, `/identities/${identityId}/locations`)
  return w.locations
}
