// Issue #510: display_name is now the globally-unique handle, so it can
// no longer be told apart from a raw identity id by checking for a `#`
// (the old `name#1234` scheme's tell). The reliable check is the other
// way around — a raw identity id is always a UUID; anything else is a
// handle to resolve first via `api.resolveHandle`. Shared by every
// "identity id or handle" input across the Hub (Friends.vue's
// onAddFriend, Profile.vue's onBlockById, Guild.vue's onInvite) so the
// regex and the decision it drives never drift between call sites.
export const IDENTITY_ID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i

export function isIdentityId(input: string): boolean {
  return IDENTITY_ID_RE.test(input)
}
