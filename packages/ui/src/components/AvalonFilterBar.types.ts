export interface AvalonFilterBarSortOption {
  value: string
  label: string
}

// Generic search/filter (+ optional sort) row: takes a query string and an
// optional sort selection as data, emits changes back up, and knows
// nothing about what it's filtering — filtering/sorting logic stays in
// whichever page or composable owns the actual list (guild roster, guild
// list, friend list, ...). Reused by apps/hub/src/views/Guild.vue's member
// roster and Guilds.vue's guild list rather than each hand-rolling its own
// text input.
export interface AvalonFilterBarProps {
  query: string
  placeholder?: string
  label?: string
  // Omit entirely to render just the search input with no sort control.
  sortOptions?: AvalonFilterBarSortOption[]
  sortValue?: string
}
