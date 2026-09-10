// Issue #270: the game directory's clickable summary card, same "props in,
// select event out, caller owns routing" shape AvalonGuildCard already
// established for guild discovery.
export interface AvalonGameCardProps {
  name: string
  slug: string
  developer: string
  // A game's `status` (crates/server/src/games.rs::GameSummary) is an
  // open-ended string, not a closed enum on the wire — only "active" is
  // producible today (see games.rs's own doc comment: #84 hasn't landed
  // key rotation/suspension/revocation yet), but the badge below treats
  // anything other than exactly "active" as visibly distinct rather than
  // assuming the full active/suspended/revoked/deprecated vocabulary
  // docs/architecture/game-registry.md sketches.
  status: string
  registeredAt: string
}
