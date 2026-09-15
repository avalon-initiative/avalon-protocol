export interface AvalonConnectionCardGrant {
  capability: string
  description: string
}

// One IntegratorBinding (#83) + its currently-active grants (#27), for the
// connected-integrators view. Data comes in as props only — no fetch/token
// awareness here, matching every other row/card component's pattern.
export interface AvalonConnectionCardProps {
  integratorName: string
  slug: string
  establishedAt: string
  grants: AvalonConnectionCardGrant[]
}
