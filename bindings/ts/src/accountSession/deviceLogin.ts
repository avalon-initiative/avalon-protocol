// AvalonClient.startAccountDeviceLogin() — the AccountSession-returning
// counterpart to the integrator IntegratorSession's own device-login
// wrapper around cross-device pairing
// (crates/server/src/device_pairing.rs). For a client with no WebAuthn
// surface of its own (a game engine, a headless client) to originate a
// first-party account login.
//
// The resulting AccountSession holds no local signing key — the approving
// device is a different device with its own key — so every
// signature-required method on it sends its request unsigned until this
// device separately requests and gets approved for its own signing key via
// requestDeviceGrant, same story resumeAccountSession already carries.
import { request } from '../http.js'
import { DeviceLoginDeniedError, DeviceLoginExpiredError, ProtocolError } from '../errors.js'
import type { AccountSession } from './core.js'
import type { components } from '../generated.js'

const DEFAULT_POLL_INTERVAL_SECONDS = 5
const MAX_POLL_INTERVAL_SECONDS = 60

type StartPairingResponseWire = components['schemas']['StartPairingResponse']
type PollPairingResponseWire = components['schemas']['PollPairingResponse']

/** A pending cross-device pairing for an AccountSession login. Mirrors
 * the Rust/C# `AccountDeviceLogin` field-for-field. */
export class AccountDeviceLogin {
  /** @internal */ private readonly serverUrl: string
  /** @internal */ private readonly deviceCode: string
  /** @internal */ private readonly resumeAccountSession: (token: string) => Promise<AccountSession>
  /** Short, human-typeable code to show the user. */
  readonly userCode: string
  /** Where the user completes approval on a WebAuthn-capable device. */
  readonly verificationUri: string
  /** Seconds until this pairing expires if it's never approved. */
  readonly expiresIn: number
  /** @internal */ private pollIntervalSeconds: number

  constructor(init: {
    serverUrl: string
    deviceCode: string
    userCode: string
    verificationUri: string
    expiresIn: number
    pollIntervalSeconds: number
    resumeAccountSession: (token: string) => Promise<AccountSession>
  }) {
    this.serverUrl = init.serverUrl
    this.deviceCode = init.deviceCode
    this.userCode = init.userCode
    this.verificationUri = init.verificationUri
    this.expiresIn = init.expiresIn
    this.pollIntervalSeconds = init.pollIntervalSeconds
    this.resumeAccountSession = init.resumeAccountSession
  }

  /** Drives `POST /auth/device/poll` to completion — doubling backoff on
   * `slow_down`, capped at `MAX_POLL_INTERVAL_SECONDS`. Resolves to a real
   * `AccountSession` on `approved`, or throws a typed error on
   * `denied`/`expired`. */
  async wait(): Promise<AccountSession> {
    let interval = Math.max(this.pollIntervalSeconds, 1)

    for (;;) {
      await sleep(interval * 1000)

      const body = await request<PollPairingResponseWire>(this.serverUrl, '/auth/device/poll', {
        method: 'POST',
        token: this.deviceCode,
      })

      switch (body.status) {
        case 'pending':
          continue
        case 'slow_down':
          interval = Math.min(interval * 2, MAX_POLL_INTERVAL_SECONDS)
          continue
        case 'denied':
          throw new DeviceLoginDeniedError()
        case 'approved': {
          if (!body.token) {
            throw new ProtocolError('approved poll response was missing a token')
          }
          return this.resumeAccountSession(body.token)
        }
        default:
          throw new DeviceLoginExpiredError()
      }
    }
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

export async function startAccountDeviceLoginRequest(
  serverUrl: string,
  resumeAccountSession: (token: string) => Promise<AccountSession>,
): Promise<AccountDeviceLogin> {
  const body = await request<StartPairingResponseWire>(serverUrl, '/auth/device/start', { method: 'POST' })
  return new AccountDeviceLogin({
    serverUrl,
    deviceCode: body.device_code,
    userCode: body.user_code,
    verificationUri: body.verification_uri,
    expiresIn: body.expires_in,
    pollIntervalSeconds: body.poll_interval > 0 ? body.poll_interval : DEFAULT_POLL_INTERVAL_SECONDS,
    resumeAccountSession,
  })
}
