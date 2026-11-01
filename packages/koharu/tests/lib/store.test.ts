import { describe, expect, it } from 'vitest'

import { defaultShortcuts, receiveDownload, useKoharuStore } from '@/lib/store'

describe('editor state', () => {
  it('retains failed downloads until they are dismissed', () => {
    useKoharuStore.setState({ downloads: {} })
    receiveDownload({
      state: 'failed',
      id: 9,
      name: 'weights.bin',
      completed: 0,
      total: 0,
      error: 'network unavailable',
    })
    expect(useKoharuStore.getState().downloads[9]).toMatchObject({
      state: 'failed',
      name: 'weights.bin',
    })

    useKoharuStore.getState().dismissDownload(9)
    expect(useKoharuStore.getState().downloads[9]).toBeUndefined()
  })

  it('defaults paint brush color to pure white (#FFFFFF)', () => {
    expect(useKoharuStore.getState().brush.color).toBe('#FFFFFF')
  })

  it('saves custom shortcuts and restores them after a restart', async () => {
    useKoharuStore.getState().setShortcut('eraser', 'X')
    expect(JSON.parse(localStorage.getItem('koharu-shortcuts')!).state).toEqual({
      shortcuts: { ...defaultShortcuts, eraser: 'x' },
    })

    useKoharuStore.setState({ shortcuts: defaultShortcuts })
    localStorage.setItem(
      'koharu-shortcuts',
      JSON.stringify({ state: { shortcuts: { eraser: 'x' } }, version: 0 }),
    )
    await useKoharuStore.persist.rehydrate()

    expect(useKoharuStore.getState().shortcuts).toEqual({ ...defaultShortcuts, eraser: 'x' })
  })

  it('shows the inpaint mask only while the eraser is active', () => {
    const { setTool } = useKoharuStore.getState()
    setTool('eraser')
    expect(useKoharuStore.getState().inpaintMaskVisible).toBe(true)
    setTool('select')
    expect(useKoharuStore.getState().inpaintMaskVisible).toBe(false)
  })

  it('keeps a mask view the user chose when leaving the eraser', () => {
    const { setTool, setInpaintMaskVisible } = useKoharuStore.getState()
    setInpaintMaskVisible(true)
    setTool('eraser')
    setTool('draw')
    expect(useKoharuStore.getState().inpaintMaskVisible).toBe(true)

    setInpaintMaskVisible(false)
    setTool('eraser')
    setInpaintMaskVisible(false)
    setInpaintMaskVisible(true)
    setTool('select')
    expect(useKoharuStore.getState().inpaintMaskVisible).toBe(true)
  })
})
