export interface AvalonWarningBannerProps {
  title: string
  message: string
  /** Defaults to 'warning'. 'danger' is for the total-loss case. */
  tone?: 'warning' | 'danger'
}
