import { defineConfig } from 'vitest/config'

// Opt-in live suite — only `*.live.test.ts` files, run against a real
// avalon-server (AVALON_SERVER_URL) and Postgres (AVALON_LIVE_DATABASE_URL)
// via `npm run test:live` from this package's own directory. Skipped
// entirely by the default `npm test` config above.
export default defineConfig({
  test: {
    environment: 'node',
    include: ['**/*.live.test.ts'],
    testTimeout: 30000,
    hookTimeout: 30000,
  },
})
