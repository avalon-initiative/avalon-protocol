// Issue #270: the integrator directory's clickable summary card, same "props in,
// select event out, caller owns routing" shape AvalonGuildCard already
// established for guild discovery.
export interface AvalonIntegratorCardProps {
  name: string
  slug: string
  ownerName: string
  // An integrator's `status` (crates/server/src/integrations.rs::IntegratorSummary) is an
  // open-ended string, not a closed enum on the wire — only "active" is
  // producible today (see integrators.rs's own doc comment: #84 hasn't landed
  // key rotation/suspension/revocation yet), but the badge below treats
  // anything other than exactly "active" as visibly distinct rather than
  // assuming the full active/suspended/revoked/deprecated vocabulary
  // docs/projects/backend-server/architecture/registry.md sketches.
  status: string
  registeredAt: string
}
