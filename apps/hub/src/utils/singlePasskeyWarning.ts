// #199: until guardian social recovery exists, an identity with exactly one
// registered passkey has no recovery path if it's lost. This is the single
// source of truth for "should the total-loss warning show" — driven by
// `GET /me/passkeys`'s count (api/passkeys.ts's `listPasskeys`), so it stays
// correct whether the count went up (a passkey was added) or down (one was
// revoked) since page load. Zero also returns false: that state shouldn't be
// reachable (every identity has at least one passkey from account creation,
// crates/server/src/passkeys.rs), and this warning is specifically about the
// single-passkey case, not an error state to report here.
export function shouldShowSinglePasskeyWarning(passkeyCount: number): boolean {
  return passkeyCount === 1
}
