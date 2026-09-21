import { defineConfig } from 'vitest/config'

// Unit tests only — no jsdom needed (no browser DOM APIs are exercised by
// the crypto/wire-shape unit tests; register()/login()'s real WebAuthn
// ceremony is not unit-tested here, see docs/architecture/sdk.md).
export default defineConfig({
  test: {
    environment: 'node',
    exclude: ['**/node_modules/**', '**/*.live.test.ts'],
  },
})
