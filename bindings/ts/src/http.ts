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

  const response = await fetch(buildUrl(serverUrl, path, options.query), {
    method: options.method ?? 'GET',
    headers,
    body: options.body !== undefined ? JSON.stringify(options.body) : undefined,
  })

  if (!response.ok) {
    throw await mapErrorResponse(response)
  }

  const text = await response.text()
  if (text.length === 0) {
    return undefined as T
  }
  return JSON.parse(text) as T
}
