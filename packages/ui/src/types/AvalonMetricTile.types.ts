// One registry metric (issue #261's `{ value, definition, class }` triple,
// crates/server/src/registry.rs::MetricResponse field-for-field) rendered
// with its definition and class label — issue #270's own hard invariant:
// no metric may ever render as a bare number anywhere in the Hub. `label`
// is the display name for the metric ("Players", "Achievements issued"),
// separate from `definition` (the metric's own explanatory sentence).
export interface AvalonMetricTileProps {
  label: string
  value: number
  definition: string
  // Always "durable-derived" today (#261's own scope) — typed as a plain
  // string, not a closed union, so a future "realtime"/"self-reported"
  // class (docs/architecture/registry.md's still-open metrics) needs
  // no change here.
  metricClass: string
}
