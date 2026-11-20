import { describe, expect, it, vi } from 'vitest'

import { bordered } from '@/components/editor/TypePanel'
import { setStyle } from '@/lib/scene'
import { commands, type Layer } from '@koharu/ipc/protocol'

/** A block whose translation's characters have `stroke`, or one span marked with it. */
function block(id: string, stroke: number[] | null, marked = false): Layer {
  const chars = stroke && !marked ? { stroke } : {}
  const content = marked ? [{ insert: 'a' }, { insert: 'b', attributes: { stroke } }] : []
  return {
    id,
    kind: 'block',
    visible: true,
    locked: false,
    children: [],
    source: {},
    translation: { content, style: { chars } },
  } as unknown as Layer
}

describe('border width on several texts', () => {
  it('keeps each text its own border colour and gives one with none white', async () => {
    const red = block('red', [1, 0, 0, 1])
    const none = block('none', null)
    const spanned = block('spanned', [0, 0, 1, 1], true)
    expect([red, none, spanned].map(bordered)).toEqual([true, false, true])

    const edit = vi.spyOn(commands, 'edit').mockResolvedValue({} as never)
    await setStyle([red, none], (layer) => ({
      chars: bordered(layer) ? { stroke_width: 4 } : { stroke_width: 4, stroke: [1, 1, 1, 1] },
    }))
    expect(edit).toHaveBeenCalledOnce()
    expect(edit.mock.calls[0]?.[0].ops).toEqual([
      {
        op: 'update_layer',
        layer: 'red',
        patch: { translation: { style: { chars: { stroke_width: 4 } } } },
      },
      {
        op: 'update_layer',
        layer: 'none',
        patch: { translation: { style: { chars: { stroke_width: 4, stroke: [1, 1, 1, 1] } } } },
      },
    ])
  })
})
