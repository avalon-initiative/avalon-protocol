# ui

## What this is, in plain language

A shared box of building blocks — buttons, form fields, layout pieces — so
the Hub website and the Hub mobile/desktop app look and behave consistently
instead of two teams independently reinventing the same button.

## What this is, technically (the meta)

`packages/ui` — the shared Vue3 component library, published internally as
`@avalon/ui`, used by both [`../hub/`](../hub/README.md) and
[`../mobile-hub/`](../mobile-hub/README.md). Developed and documented in
Storybook.

**Workspace layout**: `src/components/` holds only `.vue` files (markup),
`src/styles/` holds only `.module.scss` (CSS Modules, one per component,
imported and bound via `:class` — `<style>` blocks inside `.vue` files
aren't used anywhere in this repo's frontend), `src/stories/` holds
`.stories.ts` Storybook files, `src/types/` holds `*.types.ts`, and
`src/state/` holds non-trivial extracted script logic that isn't markup.
See `src/{components,styles,stories,types}/AvalonButton.*` for the pattern
this whole package follows.

**Status:** partial, grows alongside `hub`/`mobile-hub` as they need new
shared pieces — not a complete design system built ahead of need.

## Component inventory

Every component below is `Avalon<Name>` in `src/components/`, with a
matching `.types.ts` in `src/types/`; three (`AvalonCalendarMonth`,
`AvalonDateTimeField`, `AvalonEditableField`) have enough non-trivial script
logic to also have their own file in `src/state/`.

**Primitives** — `AvalonButton`, `AvalonCard`, `AvalonModal`, `AvalonIcon`,
`AvalonAvatar`, `AvalonTextField`, `AvalonDateTimeField`, `AvalonForm`,
`AvalonEditableField`, `AvalonColorPicker`, `AvalonFilterBar`,
`AvalonWarningBanner`, `AvalonMetricTile`.

**Navigation** — `AvalonSidebarNav`, `AvalonBottomNav`.

**Identity and social** — `AvalonAuthCard`, `AvalonUserChip`,
`AvalonPresenceBadge`, `AvalonFriendRow`, `AvalonFriendRequestRow`,
`AvalonSuggestionRow`, `AvalonCapabilityConsentRow`.

**Guilds** — `AvalonGuildCard`, `AvalonGuildMemberRow`, `AvalonRoleBadge`,
`AvalonChannelList`, `AvalonChatMessage`, `AvalonChatComposer`.

**Events** — `AvalonEventCard`, `AvalonCalendarMonth`, `AvalonRsvpControl`,
`AvalonRsvpRosterPanel`.

**Achievements and integrators** — `AvalonAchievementCard`,
`AvalonIntegratorCard`, `AvalonConnectionCard`.

Each has a Storybook story under `src/stories/` — the fastest way to see a
component's states and props without wiring up the app around it.

## Find your door

| I am... | Start here |
|---|---|
| Building a Hub view and need an existing piece | The component inventory above, then that component's `.stories.ts` for usage |
| Adding a new shared component | Follow the `AvalonButton.*` pattern — `.vue` in `components/`, `.module.scss` in `styles/`, `.types.ts` in `types/`, `.stories.ts` in `stories/`, and `.state.ts` in `state/` only if the script logic is non-trivial |

## Related projects

- [`../hub/`](../hub/README.md) and [`../mobile-hub/`](../mobile-hub/README.md) —
  the two consumers of this library.
