import { afterEach, describe, expect, it } from 'vitest'

import { useKoharuStore } from '@/lib/store'

afterEach(() => {
  localStorage.clear()
})

const restored = async (saved: unknown) => {
  localStorage.setItem('koharu-shortcuts', JSON.stringify({ state: saved, version: 0 }))
  await useKoharuStore.persist.rehydrate()
  return useKoharuStore.getState().processingStages
}

describe('the stages a run does', () => {
  it('outlive a restart, in the pipeline order, without stages that are gone', async () => {
    expect(await restored({ processingStages: ['inpaint', 'gone', 'translate'] })).toEqual([
      'translate',
      'inpaint',
    ])
  })

  it('keep their default when none that exist were saved', async () => {
    const before = useKoharuStore.getState().processingStages
    expect(await restored({ processingStages: ['gone'] })).toEqual(before)
    expect(await restored({})).toEqual(before)
  })
})
