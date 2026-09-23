export interface AvalonConnectionCardGrant {
  capability: string
  description: string
}

// One IntegratorBinding + its currently-active grants, for the
// connected-integrators view. Data comes in as props only — no fetch/token
// awareness here, matching every other row/card component's pattern.
export interface AvalonConnectionCardProps {
  integratorName: string
  slug: string
  establishedAt: string
  grants: AvalonConnectionCardGrant[]
}
