// Mirrors crates/sdk/src/lib.rs's `SdkError` — every outcome a call against
// a real avalon-server can produce, protocol-level rather than a raw
// fetch Response a caller would have to know HTTP to interpret.

export class AvalonSdkError extends Error {}

/** The session token itself was rejected (expired, unknown, malformed). */
export class UnauthorizedError extends AvalonSdkError {
  constructor() {
    super('unauthorized')
    this.name = 'UnauthorizedError'
  }
}

/** The session token is fine but this integrator/action isn't granted what
 * it needs — thrown client-side by `IntegratorSession`'s own capability
 * check before any request is made, or surfaced from a 403 response. */
export class CapabilityNotGrantedError extends AvalonSdkError {
  constructor(public readonly capability: string) {
    super(`capability not granted: ${capability}`)
    this.name = 'CapabilityNotGrantedError'
  }
}

/** A referenced resource doesn't exist — carries the server's own message. */
export class NotFoundError extends AvalonSdkError {
  constructor(message: string) {
    super(`not found: ${message}`)
    this.name = 'NotFoundError'
  }
}

/** The request conflicts with existing state. */
export class ConflictError extends AvalonSdkError {
  constructor(message: string) {
    super(`conflict: ${message}`)
    this.name = 'ConflictError'
  }
}

/** The server made a final decision not to apply the request — malformed
 * input, a signature that doesn't verify, a business-rule violation. Never
 * worth retrying unchanged. */
export class RejectedError extends AvalonSdkError {
  constructor(public readonly reason: string) {
    super(`rejected: ${reason}`)
    this.name = 'RejectedError'
  }
}

/** A connection error, timeout, or 5xx — a node hiccup, not this request
 * being wrong. */
export class UnavailableError extends AvalonSdkError {
  constructor(public readonly detail: string) {
    super(`avalon-server unavailable: ${detail}`)
    this.name = 'UnavailableError'
  }
}

/** The response didn't parse as the shape this call expected. */
export class ProtocolError extends AvalonSdkError {
  constructor(message: string) {
    super(`unexpected response from avalon-server: ${message}`)
    this.name = 'ProtocolError'
  }
}

/** The server rejected a conversation read/send with "not a participant" —
 * deliberately carries nothing beyond that (issue #97: never reveal a
 * block, not even indirectly). */
export class NotConversationParticipantError extends AvalonSdkError {
  constructor() {
    super('not a participant in this conversation')
    this.name = 'NotConversationParticipantError'
  }
}

/** `IntegratorSession.issueAchievement` needs this integrator's own slug
 * and signing key configured — thrown, without any HTTP call, when a
 * caller reaches for issuance without having supplied them. */
export class MissingIssuerCredentialsError extends AvalonSdkError {
  constructor() {
    super("this integrator's slug/signingKey were not configured")
    this.name = 'MissingIssuerCredentialsError'
  }
}

/** A device-pairing/cross-device login was explicitly denied from the
 * approving device. */
export class DeviceLoginDeniedError extends AvalonSdkError {
  constructor() {
    super('device pairing was denied')
    this.name = 'DeviceLoginDeniedError'
  }
}

/** A device-pairing/cross-device login's TTL elapsed before it was
 * approved or denied. */
export class DeviceLoginExpiredError extends AvalonSdkError {
  constructor() {
    super('device pairing expired before it was approved')
    this.name = 'DeviceLoginExpiredError'
  }
}

/** A signature-required `AccountSession` method was called on a session
 * with no local signing key. */
export class NoLocalSigningKeyError extends AvalonSdkError {
  constructor(method: string) {
    super(`${method} requires a local signing key — this AccountSession has none`)
    this.name = 'NoLocalSigningKeyError'
  }
}

/** Translates a non-success `Response` into the matching typed error,
 * mirroring `crate::http::map_error_response`. Reads the body once as
 * `{ error?: string, code?: string }`, falling back to status text. */
export async function mapErrorResponse(response: Response): Promise<AvalonSdkError> {
  let serverMessage: string | undefined
  let code: string | undefined
  try {
    const body = (await response.json()) as { error?: string; code?: string }
    serverMessage = body.error
    code = body.code
  } catch {
    // Non-JSON or empty body — fall through to a status-only message.
  }
  const message = code ?? serverMessage ?? response.statusText ?? `HTTP ${response.status}`

  switch (response.status) {
    case 401:
      return new UnauthorizedError()
    case 403:
      return new CapabilityNotGrantedError(message)
    case 404:
      return new NotFoundError(message)
    case 409:
      return new ConflictError(message)
    case 400:
    case 422:
      return new RejectedError(message)
    default:
      if (response.status >= 500) {
        return new UnavailableError(message)
      }
      return new ProtocolError(message)
  }
}
