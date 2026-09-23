import { request } from './http.js'
import type { NodeStatusResponse } from './types.js'

/** `GET /nodes/status` — public, unauthenticated. */
export function getNodeStatus(serverUrl: string): Promise<NodeStatusResponse> {
  return request<NodeStatusResponse>(serverUrl, '/nodes/status')
}
