export interface AvalonTabItem {
  label: string
  to: string
  active: boolean
}

export interface AvalonTabsProps {
  tabs: AvalonTabItem[]
}
