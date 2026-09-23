import vue from '@vitejs/plugin-vue'
import type { StorybookConfig } from '@storybook/vue3-vite'

const config: StorybookConfig = {
  stories: ['../src/stories/*.stories.ts'],
  framework: '@storybook/vue3-vite',
  viteFinal: (viteConfig) => ({ ...viteConfig, plugins: [...(viteConfig.plugins ?? []), vue()] }),
}
export default config
