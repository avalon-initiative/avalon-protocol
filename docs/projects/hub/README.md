# hub

## What this is, in plain language

Most of Avalon happens inside whatever game or app you're using — you don't
normally need a separate place to see your friends list or your
achievements. The Hub is that separate place anyway: a website where you can
manage your Avalon identity, see your friends and guilds, and look at your
achievements across every game and app you've connected, even with nothing
else open. Think of it as the one lobby that sits outside every game.

## What this is, technically (the meta)

`apps/hub` — a Vue3 + Vite + TypeScript web client, and **a client of the
network like any other — it has no backend of its own.** Every read and
write it does goes through the same SDK surface (`@avalon/sdk`) that a
game or app would use.

**Status:** a real, persistent shell with nested routed pages — not a stub
or a design mockup. Identity setup (passkeys, devices, guardian recovery),
friends/blocks/presence/discovery, guilds (roles, channels, chat, events),
achievements, and integrator discovery/connections are all implemented,
not placeholder screens — see `apps/hub/src/views/` for the current page
list (`Home`, `Friends`, `Guild(s)`, `Achievements`, `Messages`, `Profile`,
`IntegrationDirectory`, `Connections`, `CrossNodeLogin`, `RecoverIdentity`,
`PairDevice`, ...).

## Find your door

| I am... | Start here |
|---|---|
| A user of a game/app/service that integrates Avalon, wanting to understand the Hub | [`for-users.md`](for-users.md) |
| Contributing code to the Hub itself | [`architecture/hub.md`](architecture/hub.md) |
| Building the desktop/mobile companion instead | [`../hub-app/README.md`](../hub-app/README.md) |
| Looking for the shared component library the Hub is built from | [`avalon-common-ui`](https://github.com/avalon-initiative/avalon-common-ui) |

## In this folder

- [`architecture/hub.md`](architecture/hub.md) — the normative reference:
  the Hub's role in the network, what it is and isn't responsible for, and
  what's built today.
- [`for-users.md`](for-users.md) — plain-language guide to what you can do
  in the Hub.

## Related projects

- [`../backend-server/`](../backend-server/README.md) — everything the Hub
  reads and writes goes through this; the Hub has no backend of its own.
- [`avalon-common-ui`](https://github.com/avalon-initiative/avalon-common-ui) — the shared Vue3 component library the Hub is
  built from, alongside [`../hub-app/`](../hub-app/README.md).
