// Thin fetch wrapper shared by AccountSession/IntegratorSession/AvalonClient
// — no retry/backoff policy (unlike the Rust SDK's `RetryConfig`): a
// browser/Node fetch caller is expected to apply its own retry policy at a
// higher layer if it wants one; this keeps the wire layer simple and
// dependency-free.
import { mapErrorResponse } from './errors.js'

export interface RequestOptions {
  method?: 'GET' | 'POST' | 'PATCH' | 'PUT' | 'DELETE'
  body?: unknown
  token?: string
  query?: Record<string, string>
  headers?: Record<string, string>
  /** AccountSession-only: called once on a 401 against
   * `token`, to mint a replacement bearer token to retry with. Returning
   * `null` (or omitting this entirely) leaves the 401 to propagate as-is —
   * `IntegratorSession`'s own call sites never pass this, since an
   * integrator credential has no local signing key to reconnect from. */
  onUnauthorized?: (failedToken: string) => Promise<string | null>
}

function buildUrl(serverUrl: string, path: string, query?: Record<string, string>): string {
  const url = new URL(path, serverUrl)
  if (query) {
    for (const [key, value] of Object.entries(query)) {
      url.searchParams.set(key, value)
    }
  }
  return url.toString()
}

/** Issues one HTTP call and returns the parsed JSON body, or `undefined`
 * for a handler that returns an empty 200 body (axum's `Result<(), _>`
 * convention — same generic-empty-body handling
 * packages/api-client/src/client.ts::request does). Throws a typed error
 * (see errors.ts) on any non-success response. */
export async function request<T>(serverUrl: string, path: string, options: RequestOptions = {}): Promise<T> {
  const headers: Record<string, string> = { ...options.headers }
  if (options.body !== undefined) {
    headers['content-type'] = 'application/json'
  }
  if (options.token) {
    headers['authorization'] = `Bearer ${options.token}`
  }

  const method = options.method ?? 'GET'
  const requestBody = options.body !== undefined ? JSON.stringify(options.body) : undefined

  let response = await fetch(buildUrl(serverUrl, path, options.query), { method, headers, body: requestBody })

  // Issue #525: exactly one retry, with a freshly-minted continuation
  // token in place of the stale bearer one — never persisted back into
  // the session's own token (a continuation token is single-use/
  // short-lived by design, see crypto/continuation.ts), so a later
  // request against a node that still doesn't recognize the original
  // token mints its own fresh one again here, same as this one just did.
  if (!response.ok && response.status === 401 && options.token && options.onUnauthorized) {
    const reconnectToken = await options.onUnauthorized(options.token)
    if (reconnectToken) {
      response = await fetch(buildUrl(serverUrl, path, options.query), {
        method,
        headers: { ...headers, authorization: `Bearer ${reconnectToken}` },
        body: requestBody,
      })
    }
  }

  if (!response.ok) {
    throw await mapErrorResponse(response)
  }

  const text = await response.text()
  if (text.length === 0) {
    return undefined as T
  }
  return JSON.parse(text) as T
}
