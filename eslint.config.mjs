// Shared flat config (ESLint 9+) for every JS/TS/Vue package in this
// workspace (apps/hub, packages/api-client) — a single root config, picked up by
// `eslint .` run from either package's own directory. apps/mobile-hub has
// no `lint` script yet (issue #195's own scope is hub only).
import js from '@eslint/js'
import tseslint from 'typescript-eslint'
import vue from 'eslint-plugin-vue'
import globals from 'globals'

export default tseslint.config(
  {
    ignores: [
      '**/dist/**',
      '**/node_modules/**',
      '**/storybook-static/**',
      '**/coverage/**',
      'apps/hub/src/generated/**',
      'target/**',
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  // `flat/essential` only — the `strongly-recommended`/`recommended` tiers
  // are pure formatting opinions (attribute order, line-break placement)
  // that don't match this codebase's existing style and would turn "add a
  // working lint step" into "reformat ~14k lines of already-correct Vue."
  // `essential` is the correctness tier: real template bugs, not style.
  ...vue.configs['flat/essential'],
  {
    files: ['**/*.{js,mjs,cjs,ts,vue}'],
    languageOptions: {
      globals: {
        ...globals.browser,
        ...globals.node,
        // Footer build info injected by apps/hub/vite.config.ts's `define`.
        __BUILD_REVISION__: 'readonly',
        __BUILD_IS_RELEASE__: 'readonly',
      },
      parserOptions: {
        parser: tseslint.parser,
      },
    },
    rules: {
      // Repo convention (.claude/CLAUDE.md): no <style> blocks in .vue
      // files, ever — styling lives in a co-located .module.scss. This is
      // the exact convention violation this ticket asked lint to catch,
      // not just rely on manual review for.
      'vue/no-restricted-block': ['error', { element: 'style' }],
      // <script setup> stays glue-only per the same convention — multi-word
      // component name enforcement isn't part of that, so it's off rather
      // than fighting single-word view names like Home.vue/Guild.vue.
      'vue/multi-word-component-names': 'off',
      '@typescript-eslint/no-unused-vars': ['warn', { argsIgnorePattern: '^_' }],
      // vite.config.ts's `/// <reference types="vitest/config" />` is the
      // correct, necessary syntax for pulling in an ambient type
      // augmentation (not a stray import the author forgot to convert) —
      // `types: 'always'` tells the rule to allow exactly that case.
      '@typescript-eslint/triple-slash-reference': ['error', { types: 'always' }],
    },
  },
)
