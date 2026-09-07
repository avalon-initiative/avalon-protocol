// Lets TypeScript type-check `import styles from './X.module.scss'` — Vite
// handles the actual CSS Modules transform at build time.
declare module '*.module.scss' {
  const classes: Record<string, string>
  export default classes
}
