// AvalonEditableField: the read-only-until-Edit rule every logged-in page
// follows. Lives here for the same reason ui-components.test.ts does —
// packages/ui has no test runner of its own.
import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import { AvalonEditableField } from '@avalon/ui'

function mountField(props: Partial<InstanceType<typeof AvalonEditableField>['$props']> = {}) {
  return mount(AvalonEditableField, {
    props: { label: 'Display name', value: 'Nova', ...props },
    attachTo: document.body,
  })
}

describe('AvalonEditableField', () => {
  it('is read-only by default: shows the value, input hidden not interactive', () => {
    const wrapper = mountField()
    expect(wrapper.text()).toContain('Nova')
    // The input stays mounted (v-show, not v-if — less DOM churn on a
    // field toggled often) but must be genuinely hidden — `display:none`
    // via v-show, which also takes it out of the tab order natively.
    expect(wrapper.get('input').isVisible()).toBe(false)
    expect(wrapper.text()).toContain('Edit')
  })

  it('shows the empty text, muted, when there is no value', () => {
    const wrapper = mountField({ value: '', emptyText: 'Unlabeled device' })
    expect(wrapper.text()).toContain('Unlabeled device')
  })

  it('pressing Edit reveals an input pre-filled with the current value', async () => {
    const wrapper = mountField()
    await wrapper.get('button').trigger('click')
    const input = wrapper.get('input')
    expect((input.element as HTMLInputElement).value).toBe('Nova')
  })

  it('Enter saves the trimmed new value and leaves edit mode', async () => {
    const wrapper = mountField()
    await wrapper.get('button').trigger('click')
    await wrapper.get('input').setValue('  Nova Prime ')
    await wrapper.get('input').trigger('keydown', { key: 'Enter' })
    expect(wrapper.emitted('save')).toEqual([['Nova Prime']])
    expect(wrapper.get('input').isVisible()).toBe(false)
  })

  it('Escape reverts without saving', async () => {
    const wrapper = mountField()
    await wrapper.get('button').trigger('click')
    await wrapper.get('input').setValue('changed')
    await wrapper.get('input').trigger('keydown', { key: 'Escape' })
    expect(wrapper.emitted('save')).toBeUndefined()
    expect(wrapper.emitted('cancel')).toHaveLength(1)
    expect(wrapper.get('input').isVisible()).toBe(false)
    expect(wrapper.text()).toContain('Nova')
  })

  it('leaving the input saves only if the value actually changed', async () => {
    const unchanged = mountField()
    await unchanged.get('button').trigger('click')
    await unchanged.get('input').trigger('blur')
    expect(unchanged.emitted('save')).toBeUndefined()

    const changed = mountField()
    await changed.get('button').trigger('click')
    await changed.get('input').setValue('Nova II')
    await changed.get('input').trigger('blur')
    expect(changed.emitted('save')).toEqual([['Nova II']])
  })

  it('the Save button commits exactly once even though the input also blurs', async () => {
    const wrapper = mountField()
    await wrapper.get('button').trigger('click')
    await wrapper.get('input').setValue('Nova III')
    const save = wrapper.findAll('button').find((b) => b.text() === 'Save')!
    await save.trigger('click')
    expect(wrapper.emitted('save')).toEqual([['Nova III']])
  })

  it('disables Edit while saving', () => {
    const wrapper = mountField({ saving: true })
    expect(wrapper.get('button').attributes('disabled')).toBeDefined()
    expect(wrapper.text()).toContain('Saving…')
  })
})
