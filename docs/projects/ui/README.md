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

**Workspace layout** (established #431, 2026-09-15): `src/components/` holds
only `.vue` files (markup), `src/styles/` holds only `.module.scss` (CSS
Modules, one per component, imported and bound via `:class` — `<style>`
blocks inside `.vue` files aren't used anywhere in this repo's frontend),
`src/stories/` holds `.stories.ts` Storybook files, `src/types/` holds
`*.types.ts`, and `src/state/` holds non-trivial extracted script logic
that isn't markup. See `src/{components,styles,stories,types}/AvalonButton.*`
for the pattern this whole package follows.

**Status (2026-09-21):** partial, grows alongside `hub`/`mobile-hub` as they
need new shared pieces — not a complete design system built ahead of need.

## Related projects

- [`../hub/`](../hub/README.md) and [`../mobile-hub/`](../mobile-hub/README.md) —
  the two consumers of this library.
