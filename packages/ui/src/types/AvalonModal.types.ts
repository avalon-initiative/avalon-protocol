export interface AvalonModalProps {
  title: string
  // Controls visibility — this component always renders its root element
  // (v-if lives in the caller, matching every other Avalon* component's
  // "props/emits only" convention) so a caller can v-if/v-show it however
  // fits; open just decides whether the backdrop+panel show.
  open: boolean
}
