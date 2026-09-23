// Orchestrates GET /identities/:id/integrator-data for
// UserProfile.vue: resolves each instance's integrator slug/name
// client-side, mirroring achievements.ts's issuer-resolution pattern —
// the response itself only carries a raw schema id and integrator_id,
// no display name.
import { getIdentityIntegratorData, getIntegrator } from '@avalon-initiative/protocol-sdk'
import type { VisibleIntegratorDataInstance } from '@avalon-initiative/protocol-sdk'
import { getServerUrl } from './serverUrl'

// Schema ids are "game:<slug>:schema:<version>"
// (crates/server/src/integrator_schemas.rs::schema_ref) — the slug is
// parsed back out of it rather than requiring a separate integrator-id
// resolution endpoint, which doesn't exist for an arbitrary batch of ids.
export function parseIntegratorSlugFromSchema(schemaId: string): string | null {
  const parts = schemaId.split(':')
  return parts.length === 4 && parts[0] === 'game' && parts[2] === 'schema' ? parts[1] : null
}

export interface PublishedIntegratorData {
  schema: string
  integratorSlug: string
  // Falls back to integratorSlug when the lookup below has no name for
  // this slug (integrator deleted/renamed since publishing, or the
  // schema id's shape was ever unexpected).
  integratorName?: string
  publishedAt: string
  fields: Record<string, unknown>
}

export function mergeIntegratorDataInstance(
  instance: VisibleIntegratorDataInstance,
  integratorNameBySlug: Map<string, string> = new Map(),
): PublishedIntegratorData {
  const integratorSlug = parseIntegratorSlugFromSchema(instance.schema) ?? instance.integratorId
  return {
    schema: instance.schema,
    integratorSlug,
    integratorName: integratorNameBySlug.get(integratorSlug),
    publishedAt: instance.publishedAt,
    fields: instance.fields,
  }
}

export async function listPublishedIntegratorData(
  identityId: string,
): Promise<PublishedIntegratorData[]> {
  const instances = await getIdentityIntegratorData(getServerUrl(), identityId)
  if (!Array.isArray(instances) || instances.length === 0) {
    return []
  }

  const slugs = [
    ...new Set(instances.map((i) => parseIntegratorSlugFromSchema(i.schema)).filter((s) => s !== null)),
  ]
  const integrators = await Promise.all(
    slugs.map((slug) => getIntegrator(getServerUrl(), slug).catch(() => null)),
  )
  const integratorNameBySlug = new Map<string, string>()
  integrators.forEach((integrator, index) => {
    if (integrator) integratorNameBySlug.set(slugs[index], integrator.name)
  })

  return instances.map((i) => mergeIntegratorDataInstance(i, integratorNameBySlug))
}
