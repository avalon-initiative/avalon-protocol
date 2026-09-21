// Integrator connect/consent (issue #27/#83) on AccountSession — an
// identity granting or revoking its own consent to an integrator. See
// crates/server/src/connections.rs.
import { AccountSession } from './core.js'

export interface IntegratorConnection {
  bindingId: string
  integratorId: string
  establishedAt: string
  grantedCapabilities: string[]
}
interface IntegratorConnectionWire {
  binding_id: string
  integrator_id: string
  established_at: string
  granted_capabilities: string[]
}

export interface ConnectionGrant {
  capability: string
  grantedAt: string
}
interface ConnectionGrantWire {
  capability: string
  granted_at: string
}

export interface MyConnection {
  bindingId: string
  integratorId: string
  slug: string
  name: string
  establishedAt: string
  grants: ConnectionGrant[]
}
interface MyConnectionWire {
  binding_id: string
  integrator_id: string
  slug: string
  name: string
  established_at: string
  grants: ConnectionGrantWire[]
}

declare module './core.js' {
  interface AccountSession {
    /** `POST /integrations/{slug}/connect` — always signed
     * (`integration.connect`, `[slug, capabilities comma-joined]`).
     * Idempotent. */
    connectIntegrator(slug: string, capabilities: string[]): Promise<IntegratorConnection>
    /** Not signature-required (revocation only narrows). */
    disconnectIntegrator(slug: string): Promise<void>
    revokeGrant(slug: string, capability: string): Promise<void>
    myConnections(): Promise<MyConnection[]>
  }
}

AccountSession.prototype.connectIntegrator = async function (
  this: AccountSession,
  slug: string,
  capabilities: string[],
): Promise<IntegratorConnection> {
  const signature = this.sign('integration.connect', [slug, capabilities.join(',')])
  const w = await this.post<IntegratorConnectionWire>(`/integrations/${slug}/connect`, {
    capabilities,
    ...signature,
  })
  return {
    bindingId: w.binding_id,
    integratorId: w.integrator_id,
    establishedAt: w.established_at,
    grantedCapabilities: w.granted_capabilities,
  }
}

AccountSession.prototype.disconnectIntegrator = async function (this: AccountSession, slug: string): Promise<void> {
  await this.del(`/integrations/${slug}/connect`)
}

AccountSession.prototype.revokeGrant = async function (
  this: AccountSession,
  slug: string,
  capability: string,
): Promise<void> {
  await this.del(`/integrations/${slug}/grants/${capability}`)
}

AccountSession.prototype.myConnections = async function (this: AccountSession): Promise<MyConnection[]> {
  const w = await this.get<MyConnectionWire[]>('/me/connections')
  if (!Array.isArray(w)) return w as unknown as MyConnection[]
  return w.map((c) => ({
    bindingId: c.binding_id,
    integratorId: c.integrator_id,
    slug: c.slug,
    name: c.name,
    establishedAt: c.established_at,
    grants: c.grants.map((g) => ({ capability: g.capability, grantedAt: g.granted_at })),
  }))
}
