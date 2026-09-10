# Deploying avalon-server behind TLS

`avalon-server` speaks plain HTTP only — there is no native TLS listener in
the Rust app, and that's deliberate (see [Decisions and tickets](#decisions-and-tickets)).
On `localhost` that's a non-issue: loopback traffic never leaves the machine.
The moment `avalon-server` is reachable over any real network, it isn't —
login and registration send credentials in the request body, and nothing but
TLS protects that in transit (client-side hashing wouldn't help; the hash
would just become the new plaintext secret). **Terminate TLS at a reverse
proxy in front of `avalon-server`, and never expose its own port directly.**

This is standard practice, not something specific to this project: the app
binds to loopback, the proxy holds the certificate and the public port, and
the two talk plain HTTP over `localhost`.

## Recommended proxy: Caddy

Either nginx or Caddy works. This doc recommends **Caddy** as the default for
self-hosted `avalon-server` instances:

- It auto-provisions and renews Let's Encrypt (or ZeroSSL) certificates with
  no separate `certbot` setup, cron job, or renewal hook to maintain.
  Pointing a domain at the box and writing a handful of lines is enough to
  get a valid cert.
- Its config format (a `Caddyfile`) is short enough to keep inline in this
  doc and in a deployment repo, which matters for a project that wants
  self-hosting to stay low-friction (see [`../architecture/self-hosting.md`](../architecture/self-hosting.md)).
- HTTP→HTTPS redirection and modern TLS defaults are on by default, not
  something the operator has to opt into correctly.

nginx remains a perfectly good choice if an operator already runs nginx for
other services on the same box (an example config is included below too) —
this is a recommendation, not a requirement enforced by the code.

## Caddy example

```caddyfile
# /etc/caddy/Caddyfile
avalon.example.com {
    reverse_proxy 127.0.0.1:8080

    header {
        Strict-Transport-Security "max-age=31536000; includeSubDomains"
    }
}
```

That's the whole config. Caddy fetches and renews the certificate for
`avalon.example.com` automatically on first request/renewal, redirects plain
HTTP to HTTPS, and forwards `X-Forwarded-For`/`X-Forwarded-Proto` to the
upstream by default (`avalon-server` doesn't currently key any logic off
either header — see [Server-side awareness](#server-side-awareness-of-running-behind-a-proxy) below — but
they're there if a future feature needs them).

If `avalon-server` runs behind a load balancer instead of directly behind
Caddy (e.g. the multi-machine POC deployment tracked under milestone "POC
Launch", epic [#301](https://github.com/LunarVagabond/avalon-protocol/issues/301)),
put Caddy (or nginx) on each app node terminating TLS for that node, with the
load balancer either doing TCP passthrough on 443 or itself terminating TLS
and re-encrypting (or using plain HTTP) to each node over a private network —
the requirement is just that no unencrypted hop crosses a network boundary
either party doesn't control.

## nginx example

```nginx
# /etc/nginx/sites-available/avalon.example.com
server {
    listen 443 ssl http2;
    server_name avalon.example.com;

    ssl_certificate     /etc/letsencrypt/live/avalon.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/avalon.example.com/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}

server {
    listen 80;
    server_name avalon.example.com;
    return 301 https://$host$request_uri;
}
```

nginx doesn't provision certificates itself; pair it with `certbot` (or an
equivalent ACME client) to obtain and renew them under
`/etc/letsencrypt/live/avalon.example.com/`.

## Configuring avalon-server for this setup

No code changes are required for `avalon-server` to run correctly behind
either proxy — the app never inspects the client's raw TCP peer address for
anything (no IP-based rate limiting, no logic branches on it today), so
there's nothing that needs to be taught to trust `X-Forwarded-For`. Two
existing env vars just need to be set to the *public* HTTPS values rather
than left at their local-dev defaults:

- **`AVALON_SERVER_ADDR`** — bind this to loopback (`127.0.0.1:8080`, the
  default in `.env.example`) on any machine that's reachable from outside,
  not `0.0.0.0:8080`. The proxy is the only thing that should be reachable
  on the public interface; the app port should never be.
- **`AVALON_WEBAUTHN_RP_ID`** / **`AVALON_WEBAUTHN_ORIGIN`** — these must be
  the real public domain and its `https://` origin (e.g. `avalon.example.com`
  / `https://avalon.example.com`), not `localhost`. `webauthn-rs` validates
  the origin the browser reports byte-for-byte, and WebAuthn's browser
  secure-context rules require an actual TLS origin once the RP ID isn't
  `localhost` — this is a second, independent reason TLS termination has to
  be in place before a real deployment can do passkey registration/login at
  all, not just a data-protection nicety.
- **`AVALON_HUB_ORIGIN`** — same idea, set to the Hub's real deployed
  origin(s) for CORS.

None of this is new configuration surface; it's the existing
`AVALON_SERVER_ADDR`/`AVALON_WEBAUTHN_RP_ID`/`AVALON_WEBAUTHN_ORIGIN`/`AVALON_HUB_ORIGIN`
variables from `.env.example` (see
[`local-development.md`](local-development.md)) simply pointed at production
values instead of the local-dev ones.

## Today in the repo

- `avalon-server` has no TLS listener and no dependency that would give it
  one — `crates/server/src/main.rs` binds a plain `tokio::net::TcpListener`
  and serves HTTP via `axum::serve` directly. This is intentional, not an
  oversight: TLS termination belongs at the proxy layer, not duplicated
  inside the app.
- `avalon-server` does not read the client's peer address anywhere in
  `crates/server/src`, so there's no `X-Forwarded-For`/trusted-proxy
  handling to add for this ticket — if a future feature needs the real
  client IP (abuse detection, geolocation, ...), that's the point to wire up
  `axum::extract::ConnectInfo` plus explicit proxy trust, not before.
- Passwords are hashed with Argon2id at rest (`crates/server/src/auth.rs`)
  independent of all of the above — this document is about the wire, not
  storage.

## Decisions and tickets

- [#72](https://github.com/LunarVagabond/avalon-protocol/issues/72) —
  `avalon-server` must run behind TLS before any non-local deployment
  (this document)
- [#301](https://github.com/LunarVagabond/avalon-protocol/issues/301) —
  POC Launch epic; the first deployment this document's guidance applies to
  for real
