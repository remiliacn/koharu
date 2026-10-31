'use client'

import { useEffect, type RefObject } from 'react'

import type { Camera } from '@/lib/geometry'
import { useInpaintMask } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import type { Page, Point, RasterStrokeMode } from '@koharu/bridge/protocol'

// Matches the Remove brush preview drawn by koharu-canvas.
const MASK_COLOR = [168, 85, 247] as const
const MASK_OPACITY = 116 / 255

export function InpaintMaskOverlay({
  page,
  camera,
  canvas,
}: {
  page: Page
  camera: Camera
  canvas: RefObject<HTMLCanvasElement | null>
}) {
  const revision = useKoharuStore((state) => state.canvasRevision)
  const mask = useInpaintMask(page.id, revision).data

  useEffect(() => {
    const context = canvas.current?.getContext('2d')
    if (!context || !mask) return
    let current = true
    void decodeMask(mask).then((bitmap) => {
      if (current) drawMask(context, bitmap)
      bitmap?.close()
    })
    return () => {
      current = false
    }
  }, [canvas, mask])

  const dpr = window.devicePixelRatio
  return (
    <canvas
      ref={canvas}
      data-testid='inpaint-mask'
      width={page.size.width}
      height={page.size.height}
      className='absolute'
      style={{
        left: camera.translation[0] / dpr,
        top: camera.translation[1] / dpr,
        width: (page.size.width * camera.zoom) / dpr,
        height: (page.size.height * camera.zoom) / dpr,
        opacity: MASK_OPACITY,
      }}
    />
  )
}

export function drawMaskStroke(
  canvas: HTMLCanvasElement | null,
  mode: RasterStrokeMode,
  points: Point[],
  diameter: number,
): void {
  const context = canvas?.getContext('2d')
  const [first, ...rest] = points
  if (!context || !first) return
  context.save()
  context.globalCompositeOperation = mode === 'erase' ? 'destination-out' : 'source-over'
  context.strokeStyle = `rgb(${MASK_COLOR.join(' ')})`
  context.lineWidth = diameter
  context.lineCap = 'round'
  context.lineJoin = 'round'
  context.beginPath()
  context.moveTo(first.x, first.y)
  for (const point of rest.length ? rest : [first]) context.lineTo(point.x, point.y)
  context.stroke()
  context.restore()
}

async function decodeMask(bytes: Uint8Array): Promise<ImageBitmap | null> {
  if (!bytes.byteLength) return null
  return createImageBitmap(new Blob([bytes as BlobPart], { type: 'image/png' }))
}

function drawMask(context: CanvasRenderingContext2D, bitmap: ImageBitmap | null): void {
  const { width, height } = context.canvas
  context.clearRect(0, 0, width, height)
  if (!bitmap) return
  context.drawImage(bitmap, 0, 0, width, height)
  const image = context.getImageData(0, 0, width, height)
  const pixels = image.data
  for (let index = 0; index < pixels.length; index += 4) {
    pixels[index + 3] = pixels[index]
    pixels[index] = MASK_COLOR[0]
    pixels[index + 1] = MASK_COLOR[1]
    pixels[index + 2] = MASK_COLOR[2]
  }
  context.putImageData(image, 0, 0)
}
