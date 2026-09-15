export interface AvalonAvatarProps {
  /** Image URL; when absent (or empty) the first letter of `name` is shown instead. */
  src?: string | null
  /** Used for the fallback initial and the image's alt text. */
  name: string
  size?: 'sm' | 'md' | 'lg' | 'xl'
}
