import type { Preview } from '@storybook/vue3'
import '../src/styles/tokens.css'

const preview: Preview = {
  parameters: {
    backgrounds: { default: 'avalon', options: { avalon: { name: 'avalon', value: '#0b1220' } } },
  },
  initialGlobals: { backgrounds: { value: 'avalon' } },
}
export default preview
