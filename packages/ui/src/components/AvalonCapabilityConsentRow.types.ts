// One requested capability + its plain-language description, for the
// game-connect consent view (#27). Unchecked by default is enforced by the
// caller (the view owns the checked-set state, not this row) — no
// "approve all" shortcut anywhere in this component.
export interface AvalonCapabilityConsentRowProps {
  capability: string
  description: string
  checked: boolean
}
