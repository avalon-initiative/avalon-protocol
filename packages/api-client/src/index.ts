// Barrel export for @avalon/api-client (#60). apps/hub's own domain-specific
// api/*.ts modules (friends.ts, guilds.ts, achievements.ts, ...) keep living
// in apps/hub — only the fetch client, session store, WebAuthn/signing-key
// auth ceremony, and shared wire types moved here, since those are exactly
// what apps/mobile-hub also needs and neither app should duplicate.
export * from './client'
export * from './errors'
export * from './types'
export * from './identity'
export * from './session'
export * from './storage'
export * from './crypto/signingKey'
export * from './crypto/webauthn'
export * from './crypto/continuation'
