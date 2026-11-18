'use client'

import { create } from 'zustand'
import { persist } from 'zustand/middleware'

import type {
  Bounds,
  LayerId,
  Model,
  PageId,
  PipelineEvent,
  Preferences,
  Rgba,
  RunId,
  Size,
  Stage,
  Tool,
  Transfer,
  Typed,
} from '@koharu/ipc/protocol'
import { toast } from '@koharu/ui/components/toast'

export type CanvasTool = Tool
export const MIN_BRUSH_DIAMETER = 1
export function maxBrushDiameter(size: Size): number {
  // Assume that for all sizes below 2048 width, 128 is sufficient.
  // For anything above, scale by percentage greater than 2048 width.
  return Math.round(Math.max(size.width / 2048, 1.0) * 128)
}

/** What a single key does: picks a tool, fits the page to the window, or shows the masks. */
export type ShortcutAction = CanvasTool | 'fit' | 'masks'
export type Shortcuts = Record<ShortcutAction, string>

/** Each tool's key, Photoshop's for its counterpart: the Move tool's V, the Direct Selection
 * tool's A, the Type tool's T, the Brush's B, the Eraser's E, the Eyedropper's I, the Remove
 * tool's J, and the Hand's H; 0 fits the page, as Photoshop's Ctrl+0 does; and Q shows the masks,
 * as Photoshop's Quick Mask. The user can rebind each in Settings. */
export const defaultShortcuts: Shortcuts = {
  select: 'v',
  direct_select: 'a',
  marquee: 'm',
  lasso: 'l',
  text: 't',
  draw: 'b',
  eraser: 'e',
  color_picker: 'i',
  remove: 'j',
  pan: 'h',
  fit: '0',
  masks: 'q',
}

export interface CanvasBrush {
  diameter: number
  color: string
}

/** Where the page shows: a page point `p` sits at `p * zoom + (x, y)` in CSS pixels from the
 * canvas element's corner, as the native canvas publishes it. */
export interface Camera {
  zoom: number
  x: number
  y: number
}

export type PipelineScope = 'selected-pages' | 'project'
/** The stages as the interface lists them, translation before inpainting as the fork listed them;
 * a run still takes them in the pipeline's own order. */
export const pipelineStages: readonly Stage[] = ['detect', 'recognize', 'translate', 'inpaint']

/** A pipeline run as the activity center shows it, from the pipeline's events. */
export interface Job {
  id: RunId
  state: 'running' | 'done' | 'failed' | 'stopped'
  /** The page stages done, of `total`. */
  completed: number
  total: number
  /** The stage started last, and its page. */
  stage: Stage | null
  page: PageId | null
  error: string | null
}

interface KoharuStore {
  initialized: boolean
  preferences: Preferences | null
  models: Model[]
  jobs: Record<string, Job>
  downloads: Record<string, Transfer>
  camera: Camera
  /** Where the area the canvas's marquee and lasso marked lies on the page. */
  area: Bounds | null
  /** The text the canvas is typing into, and its selected characters' style. */
  typing: Typed | null
  /** The colour the canvas's picker picked last, numbered so a repeat counts. */
  picked: { color: string; serial: number } | null
  selectedLayers: LayerId[]
  /** The selection the canvas has, so one it published isn't sent back to it. */
  canvasSelection: LayerId[]
  selectedPages: PageId[]
  tool: CanvasTool
  brush: CanvasBrush
  /** Whether the page's inpainting masks show on the canvas, taking the brush and the eraser. */
  masksShown: boolean
  /** That the eraser showed the masks, which leaving it hides again. */
  eraserShowedMasks: boolean
  processingScope: PipelineScope
  processingStages: Stage[]
  settingsOpen: boolean
  /** Whether the History window shows over the canvas. */
  historyOpen: boolean
  shortcuts: Shortcuts
  selectPages: (pages: PageId[]) => void
  setProcessingScope: (scope: PipelineScope) => void
  setProcessingStages: (stages: Stage[]) => void
  setSettingsOpen: (open: boolean) => void
  setHistoryOpen: (open: boolean) => void
  setShortcut: (action: ShortcutAction, key: string) => void
  selectLayers: (layers: LayerId[]) => void
  setTool: (tool: CanvasTool) => void
  setBrush: (brush: CanvasBrush) => void
  setMasksShown: (shown: boolean) => void
  dismissJob: (id: string) => void
  dismissDownload: (id: number) => void
}

export const useKoharuStore = create<KoharuStore>()(
  persist(
    (set) => ({
      initialized: false,
      preferences: null,
      models: [],
      jobs: {},
      downloads: {},
      camera: { zoom: 1, x: 0, y: 0 },
      area: null,
      typing: null,
      picked: null,
      selectedLayers: [],
      canvasSelection: [],
      selectedPages: [],
      tool: 'select',
      brush: { diameter: 48, color: '#FFFFFF' },
      masksShown: false,
      eraserShowedMasks: false,
      processingScope: 'selected-pages',
      processingStages: [...pipelineStages],
      settingsOpen: false,
      historyOpen: false,
      shortcuts: defaultShortcuts,
      selectPages: (selectedPages) => set({ selectedPages: [...new Set(selectedPages)] }),
      setProcessingScope: (processingScope) => set({ processingScope }),
      setProcessingStages: (processingStages) => set({ processingStages }),
      setSettingsOpen: (settingsOpen) => set({ settingsOpen }),
      setHistoryOpen: (historyOpen) => set({ historyOpen }),
      setShortcut: (action, key) =>
        set((state) => ({
          shortcuts: { ...state.shortcuts, [action]: key.toLowerCase().slice(0, 1) },
        })),
      selectLayers: (selectedLayers) => set({ selectedLayers: [...new Set(selectedLayers)] }),
      // The eraser shows the masks it edits and hides them again on leaving, unless the user
      // chose whether they show meanwhile.
      setTool: (tool) =>
        set((state) => {
          if (tool === state.tool) return { tool }
          if (tool === 'eraser' && !state.masksShown) {
            return { tool, masksShown: true, eraserShowedMasks: true }
          }
          if (state.eraserShowedMasks) return { tool, masksShown: false, eraserShowedMasks: false }
          return { tool }
        }),
      setBrush: (brush) => set({ brush }),
      setMasksShown: (masksShown) => set({ masksShown, eraserShowedMasks: false }),
      dismissJob: (id) =>
        set((state) => {
          const jobs = { ...state.jobs }
          delete jobs[id]
          return { jobs }
        }),
      dismissDownload: (id) =>
        set((state) => {
          const downloads = { ...state.downloads }
          delete downloads[String(id)]
          return { downloads }
        }),
    }),
    {
      // The shortcuts outlive a restart; a binding saved for an action that's gone is dropped,
      // and an action saved before it existed keeps its default.
      name: 'koharu-shortcuts',
      partialize: (state) => ({ shortcuts: state.shortcuts }),
      merge: (persisted, current) => {
        const saved = (persisted as Partial<Pick<KoharuStore, 'shortcuts'>> | undefined)?.shortcuts
        const shortcuts = { ...current.shortcuts }
        for (const action of Object.keys(shortcuts) as ShortcutAction[]) {
          if (typeof saved?.[action] === 'string') shortcuts[action] = saved[action]
        }
        return { ...current, shortcuts }
      },
    },
  ),
)

/** Every run's events so far, as the editor loads. */
export function receiveRuns(events: PipelineEvent[]): void {
  const jobs: Record<string, Job> = {}
  for (const event of events) jobs[String(event.run)] = fold(jobs[String(event.run)], event)
  useKoharuStore.setState({ initialized: true, jobs })
}

export function receivePipeline(event: PipelineEvent): void {
  useKoharuStore.setState((state) => ({
    jobs: { ...state.jobs, [String(event.run)]: fold(state.jobs[String(event.run)], event) },
  }))
}

function fold(job: Job | undefined, event: PipelineEvent): Job {
  const current: Job = job ?? {
    id: event.run,
    state: 'running',
    completed: 0,
    total: 0,
    stage: null,
    page: null,
    error: null,
  }
  switch (event.kind) {
    case 'queued':
      return { ...current, total: event.steps }
    case 'started':
      return { ...current, stage: event.stage, page: event.page }
    case 'finished':
      return { ...current, completed: current.completed + 1 }
    case 'ended':
      // A failure names its own page and stage: the last started may be another page in flight.
      return event.outcome.kind === 'failed'
        ? {
            ...current,
            state: 'failed',
            stage: event.outcome.stage,
            page: event.outcome.page,
            error: event.outcome.reason,
          }
        : { ...current, state: event.outcome.kind, error: null }
  }
}

export function receiveDownload(download: Transfer): void {
  useKoharuStore.setState((state) => {
    const downloads = { ...state.downloads }
    if (download.state === 'Finished') delete downloads[String(download.id)]
    else downloads[String(download.id)] = download
    return { downloads }
  })
}

/** A selection the canvas made. */
export function receiveCanvasSelection(layers: LayerId[]): void {
  useKoharuStore.setState({ selectedLayers: [...new Set(layers)], canvasSelection: layers })
}

export function receivePickedColor(color: string): void {
  useKoharuStore.setState((state) => ({
    picked: { color, serial: (state.picked?.serial ?? 0) + 1 },
  }))
}

export function receivePreferences(preferences: Preferences): void {
  useKoharuStore.setState({ preferences })
}

export function receiveModels(models: Model[]): void {
  useKoharuStore.setState({ models })
}

export function receiveError(message: string): void {
  toast.add({ type: 'error', title: 'Could not complete that action', description: message })
}

/** A CSS hex colour as the canvas's straight RGBA, from 0 to 1. */
export function hexToRgba(hex: string): Rgba {
  const value = hex.replace('#', '')
  const channel = (start: number) => Number.parseInt(value.slice(start, start + 2), 16) / 255
  return [channel(0), channel(2), channel(4), 1]
}

/** The canvas's RGBA as a CSS hex colour, its alpha left out. */
export function rgbaToHex(color: Rgba): string {
  return `#${color
    .slice(0, 3)
    .map((channel) =>
      Math.round(Math.min(1, Math.max(0, channel)) * 255)
        .toString(16)
        .padStart(2, '0'),
    )
    .join('')}`.toUpperCase()
}
