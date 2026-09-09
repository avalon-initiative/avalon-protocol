import { describe, expect, it } from 'vitest'
import { shouldShowSinglePasskeyWarning } from './singlePasskeyWarning'

describe('shouldShowSinglePasskeyWarning', () => {
  it('shows when there is exactly one passkey', () => {
    expect(shouldShowSinglePasskeyWarning(1)).toBe(true)
  })

  it('hides once a second passkey is registered', () => {
    expect(shouldShowSinglePasskeyWarning(2)).toBe(false)
  })

  it('hides for any count above two as well', () => {
    expect(shouldShowSinglePasskeyWarning(5)).toBe(false)
  })

  it('hides for zero (not the case this warning is about)', () => {
    expect(shouldShowSinglePasskeyWarning(0)).toBe(false)
  })
})
