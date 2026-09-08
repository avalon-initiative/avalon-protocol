export interface AvalonConnectionCardGrant {
  capability: string
  description: string
}

// One GameBinding (#83) + its currently-active grants (#27), for the
// connected-games view. Data comes in as props only — no fetch/token
// awareness here, matching every other row/card component's pattern.
export interface AvalonConnectionCardProps {
  gameName: string
  slug: string
  establishedAt: string
  grants: AvalonConnectionCardGrant[]
}
