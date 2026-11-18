'use client'

import { AlignCenter, AlignLeft, AlignRight, ChevronDown, Italic, Minus, Plus } from 'lucide-react'
import {
  type ComponentProps,
  type ReactNode,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
} from 'react'
import { useTranslation } from 'react-i18next'

import { ColorWell } from '@/components/controls/ColorWell'
import { FontPicker } from '@/components/controls/FontPicker'
import { call, dispatch } from '@/lib/backend'
import {
  expandLayerSelection,
  isBlockLayer,
  isTextLayer,
  letteringOf,
  walkLayers,
} from '@/lib/document'
import { useFonts, usePage } from '@/lib/queries'
import { setStyle, updateLayer } from '@/lib/scene'
import { hexToRgba, rgbaToHex, useKoharuStore } from '@/lib/store'
import {
  commands,
  type Align,
  type CharKey,
  type CharStyle,
  type Direction,
  type FontFamily,
  type LayerId,
  type Text,
  type TextStyle,
  type Value,
} from '@koharu/ipc/protocol'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from '@koharu/ui/components/dropdown-menu'
import {
  NumberField,
  NumberFieldDecrement,
  NumberFieldGroup,
  NumberFieldIncrement,
  NumberFieldInput,
} from '@koharu/ui/components/number-field'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@koharu/ui/components/select'
import { Toggle } from '@koharu/ui/components/toggle'

/** The renderer's own default when `[renderer]` names no font. */
const FALLBACK_FONT = 'CCWildWords'
const BLACK = '#000000'
const WHITE = '#FFFFFF'

/** The selected texts' type settings in the Properties panel, as Photoshop's Character panel
 * shows them: every setting a text's lettering has, the style it draws with, its direction, and its
 * box's fit, disabled while no text is selected. While the canvas types, a character setting
 * styles the selected characters, or the next ones typed at a caret; otherwise every character of
 * every text and block selected. A value the characters don't share shows empty. */
export function TypePanel() {
  const { t } = useTranslation()
  const borderWidthId = useId()
  const page = usePage().data
  const selectedIds = useKoharuStore((state) => state.selectedLayers)
  const defaultFont = useKoharuStore(
    (state) => state.preferences?.renderer.font_families?.[0] ?? FALLBACK_FONT,
  )
  const availableFonts = useFonts().data
  const targets = useMemo(() => {
    if (!page) return []
    const leaves = expandLayerSelection(page.layers, selectedIds)
    return walkLayers(page.layers)
      .map(({ layer }) => layer)
      .filter((layer) => leaves.includes(layer.id) && (isTextLayer(layer) || isBlockLayer(layer)))
  }, [page, selectedIds])
  const current = targets[0]
  const lettering: Text | null = current ? letteringOf(current) : null
  const typing = useKoharuStore((state) => state.typing)
  const [draft, setDraft] = useState<{ layer: LayerId; style: TextStyle } | null>(null)
  const sequence = useRef(0)

  useEffect(() => setDraft(null), [current?.id])

  // While typing, what the canvas says its selected characters share; otherwise what every
  // character of the selected texts shares.
  const shared = useMemo(
    () => typing ?? sharedChars(targets.flatMap((layer) => letteringOf(layer) ?? [])),
    [typing, targets],
  )
  const drafted = current && draft?.layer === current.id ? draft.style : null
  const style: TextStyle = {
    ...lettering?.style,
    ...drafted,
    chars: { ...shared.chars, ...drafted?.chars },
  }
  const chars = style.chars ?? {}
  const mixed = new Set(shared.mixed.filter((key) => drafted?.chars?.[key] === undefined))
  const frame = lettering?.frame ?? 'point'
  // A box and a path fit their text alike, a balloon's translation in a path; point text has no fit.
  const fitted = typeof frame !== 'object' ? null : frame.box ? 'box' : 'path'
  const autoFit = typeof frame === 'object' && (frame.box ?? frame.path)?.fit === 'best'

  /** Merges `patch` into each target's style, shown at once on the first. */
  const apply = (patch: TextStyle) => {
    if (!current) return
    setDraft({
      layer: current.id,
      style: { ...style, ...patch, chars: { ...chars, ...patch.chars } },
    })
    const request = ++sequence.current
    void call(setStyle, targets, patch)
      .catch(() => undefined)
      .finally(() => {
        if (sequence.current === request) setDraft(null)
      })
  }

  /** A character setting: while typing, the canvas styles the selected characters, or holds it
   * for the next ones typed at a caret; otherwise every character of each target takes it. */
  const applyChars = (patch: CharStyle) => {
    if (!typing) return apply({ chars: patch })
    // A mark can't unset a value, so no border on some characters is a border no wide.
    const marked = patch.stroke === null ? { ...patch, stroke: undefined, stroke_width: 0 } : patch
    void call(commands.styleTyping, marked).catch(() => undefined)
  }

  /** Merges `patch` into each target's lettering: a text's own values, a block's translation's. */
  const update = (patch: { [key in string]: Value }) => {
    for (const layer of targets) {
      dispatch(updateLayer, layer.id, isBlockLayer(layer) ? { translation: patch } : patch)
    }
  }

  const families = useMemo(() => {
    const available = availableFonts ?? []
    return available.some((family) => sameFont(family.name, defaultFont))
      ? available
      : [...available, placeholderFamily(defaultFont)]
  }, [availableFonts, defaultFont])
  const family = mixed.has('family') ? '' : (chars.family ?? defaultFont)
  const selectedFamily = families.find((candidate) => sameFont(candidate.name, family))
  const italics = hasItalic(selectedFamily)
  const italic = Boolean(chars.italic) && italics
  const weight = chars.weight ?? 400
  const weights = usableFontWeights(selectedFamily, italic)
  const size = mixed.has('size') ? null : Math.round((chars.size ?? 24) * 100) / 100
  const strokeWidth = chars.stroke_width ?? 0
  const stroke = chars.stroke ?? null
  const strokeEnabled = stroke !== null && stroke[3] > 0 && strokeWidth > 0
  const direction: Direction = lettering?.direction ?? 'horizontal'
  const align: Align = style.align ?? (direction === 'vertical' ? 'start' : 'center')

  // A new text being typed has no layer yet for its paragraph to go to.
  const paragraph = targets.length > 0
  const disabled = !(targets.length || typing)

  const toggle =
    'h-6 w-full min-w-6 rounded-md bg-background px-0 text-muted-foreground data-[pressed]:border-primary data-[pressed]:bg-primary data-[pressed]:text-primary-foreground data-[pressed]:hover:bg-primary/90'
  return (
    <div className='grid min-w-0 gap-1.5 p-2' data-testid='type-panel' aria-disabled={disabled}>
      <div className='grid min-w-0 grid-cols-[minmax(0,1fr)_2.5rem] gap-1.5'>
        <TypeField label={t('inspector.font')}>
          <FontPicker
            value={family}
            families={families}
            disabled={disabled}
            size='sm'
            onChange={(next) => {
              const nextFamily = families.find((candidate) => sameFont(candidate.name, next))
              const nextItalic = italic && hasItalic(nextFamily)
              applyChars({
                family: next,
                italic: nextItalic,
                weight: nearestFontWeight(usableFontWeights(nextFamily, nextItalic), weight),
              })
            }}
          />
        </TypeField>
        <TypeField label={t('inspector.color')}>
          <ColorWell
            label={t('inspector.textColor')}
            size='sm'
            disabled={disabled}
            value={mixed.has('fill') ? '' : chars.fill ? rgbaToHex(chars.fill) : BLACK}
            onChange={(color) => color && applyChars({ fill: hexToRgba(color) })}
          />
        </TypeField>
      </div>

      <div className='grid min-w-0 grid-cols-[minmax(0,1fr)_4.25rem_2.5rem] gap-1.5'>
        <TypeField label={t('inspector.fontSize')}>
          <FontSizeField
            // A size typed but not applied belongs to the text it was typed for.
            key={current?.id ?? ''}
            disabled={disabled}
            value={size}
            autoFit={autoFit}
            canAutoFit={fitted !== null}
            onChange={(next) => {
              applyChars({ size: next })
              // A set size is the most the text takes, as the frame's best fit would grow it.
              if (autoFit && fitted) update({ frame: { [fitted]: { fit: 'shrink' } } })
            }}
            onAutoFit={() => fitted && update({ frame: { [fitted]: { fit: 'best' } } })}
          />
        </TypeField>
        <TypeField label={t('inspector.fontWeight')}>
          <Select
            disabled={disabled}
            value={mixed.has('weight') ? '' : String(weight)}
            onValueChange={(next) => next && applyChars({ weight: Number(next) })}
          >
            <SelectTrigger
              size='sm'
              aria-label={t('inspector.fontWeight')}
              className='w-full min-w-0'
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {weights.map((value) => (
                <SelectItem key={value} value={String(value)}>
                  {value}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </TypeField>
        <TypeField label={t('inspector.fontStyles.italic')}>
          <Toggle
            variant='outline'
            size='sm'
            aria-label={t('inspector.fontStyles.italic')}
            className={toggle}
            disabled={disabled || (!italics && !italic)}
            pressed={italic}
            onPressedChange={(nextItalic) =>
              applyChars({
                italic: nextItalic,
                weight: nearestFontWeight(usableFontWeights(selectedFamily, nextItalic), weight),
              })
            }
          >
            <Italic className='size-3.5' />
          </Toggle>
        </TypeField>
      </div>

      <div className='grid min-w-0 grid-cols-[minmax(0,1fr)_minmax(6.5rem,1fr)] gap-1.5'>
        <TypeField label={t('inspector.alignment')}>
          <div className='grid h-6 grid-cols-3 rounded-md border border-input bg-background p-px'>
            {(
              [
                [
                  'start',
                  AlignLeft,
                  direction === 'vertical' ? t('inspector.alignTop') : t('inspector.alignLeft'),
                ],
                [
                  'center',
                  AlignCenter,
                  direction === 'vertical'
                    ? t('inspector.alignMiddle')
                    : t('inspector.alignCenter'),
                ],
                [
                  'end',
                  AlignRight,
                  direction === 'vertical' ? t('inspector.alignBottom') : t('inspector.alignRight'),
                ],
              ] as const
            ).map(([value, Icon, label]) => (
              <button
                key={value}
                type='button'
                aria-label={label}
                aria-pressed={align === value}
                data-active={align === value}
                disabled={!paragraph}
                className='grid place-items-center rounded-[4px] text-muted-foreground hover:text-foreground disabled:cursor-not-allowed disabled:opacity-40 data-[active=true]:bg-primary data-[active=true]:text-primary-foreground data-[active=true]:hover:bg-primary/90'
                onClick={() => apply({ align: value })}
              >
                <Icon className='size-3' />
              </button>
            ))}
          </div>
        </TypeField>
        <TypeField label={t('inspector.textDirection')}>
          <Select
            disabled={!paragraph}
            value={direction}
            onValueChange={(next) => next && update({ direction: next })}
          >
            <SelectTrigger size='sm' aria-label={t('inspector.textDirection')} className='w-full'>
              <SelectValue>{t(`inspector.${direction}`)}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectItem value='horizontal'>{t('inspector.horizontal')}</SelectItem>
              <SelectItem value='vertical'>{t('inspector.vertical')}</SelectItem>
            </SelectContent>
          </Select>
        </TypeField>
      </div>

      <div className='grid min-w-0 grid-cols-[2.5rem_minmax(5.5rem,1fr)] items-end gap-1.5'>
        <TypeField label={t('inspector.border')}>
          <ColorWell
            label={t('inspector.borderColor')}
            size='sm'
            disabled={disabled}
            allowTransparent
            value={strokeEnabled && stroke ? rgbaToHex(stroke) : null}
            onChange={(color) =>
              applyChars(
                color === null
                  ? { stroke: null }
                  : {
                      stroke: hexToRgba(color),
                      stroke_width: strokeWidth > 0 ? strokeWidth : 1.5,
                    },
              )
            }
          />
        </TypeField>
        <TypeField label={t('inspector.borderWidth')}>
          <NumberField
            id={borderWidthId}
            name='border-width'
            className='min-w-0'
            disabled={disabled}
            value={strokeWidth > 0 ? strokeWidth : 1.5}
            min={0.5}
            max={32}
            step={0.5}
            onValueChange={(next) => {
              if (next !== null && next >= 0.5 && next <= 32) {
                applyChars({ stroke_width: next, stroke: stroke ?? hexToRgba(WHITE) })
              }
            }}
          >
            <NumberFieldGroup>
              <NumberFieldDecrement aria-label={t('inspector.decreaseBorderWidth')}>
                <Minus />
              </NumberFieldDecrement>
              <NumberFieldInput aria-label={t('inspector.borderWidth')} />
              <NumberFieldIncrement aria-label={t('inspector.increaseBorderWidth')}>
                <Plus />
              </NumberFieldIncrement>
            </NumberFieldGroup>
          </NumberField>
        </TypeField>
      </div>
    </div>
  )
}

function TypeField({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className='grid min-w-0 gap-0.5'>
      <span className='truncate text-[8px] font-medium tracking-[0.06em] text-muted-foreground uppercase'>
        {label}
      </span>
      {children}
    </div>
  )
}

/** A family the font list hasn't loaded, so the default still shows. */
function placeholderFamily(name: string): FontFamily {
  return {
    name,
    faces: [{ post_script_name: name, weight: 400, weight_range: null, italic: false }],
  }
}

const CHAR_KEYS: CharKey[] = [
  'family',
  'weight',
  'italic',
  'size',
  'fill',
  'stroke',
  'stroke_width',
]

/** What every character of `texts` shares, each span's marks over its text's style, and the values
 * they don't, which it leaves empty, as the canvas reports a typed selection's. */
function sharedChars(texts: Text[]): { chars: CharStyle; mixed: CharKey[] } {
  const styles: CharStyle[] = texts.flatMap((text) => {
    const base = text.style?.chars ?? {}
    const spans = text.content?.length ? text.content : [{ insert: '' }]
    return spans.map((span) => ({
      ...base,
      ...Object.fromEntries(
        Object.entries(span.attributes ?? {}).filter(([, value]) => value != null),
      ),
    }))
  })
  const [first, ...rest] = styles
  if (!first) return { chars: {}, mixed: [] }
  const same = (a: unknown, b: unknown) => JSON.stringify(a ?? null) === JSON.stringify(b ?? null)
  const mixed = CHAR_KEYS.filter((key) => rest.some((style) => !same(style[key], first[key])))
  const chars: CharStyle = { ...first }
  for (const key of mixed) delete chars[key]
  return { chars, mixed }
}

function hasItalic(family: FontFamily | undefined): boolean {
  return Boolean(family?.faces.some((face) => face.italic))
}

function usableFontWeights(family: FontFamily | undefined, italic: boolean): number[] {
  if (!family) return [400]
  const styled = family.faces.filter((face) => face.italic === italic)
  const faces = styled.length ? styled : family.faces
  const weights = new Set(faces.map((face) => face.weight))
  for (const face of faces) {
    if (!face.weight_range) continue
    const { start, end } = face.weight_range
    weights.add(start)
    weights.add(end)
    for (let weight = 100; weight <= 900; weight += 100) {
      if (weight >= start && weight <= end) weights.add(weight)
    }
  }
  return [...weights].sort((left, right) => left - right)
}

function nearestFontWeight(weights: number[], target: number): number {
  return weights.reduce(
    (nearest, weight) =>
      Math.abs(weight - target) < Math.abs(nearest - target) ? weight : nearest,
    weights[0] ?? target,
  )
}

function sameFont(left: string, right: string): boolean {
  const normalize = (value: string) =>
    value.normalize('NFKC').trim().replace(/\s+/g, ' ').toLowerCase()
  return normalize(left) === normalize(right)
}

const fontSizePresets = [8, 9, 10, 12, 14, 16, 18, 20, 24, 28, 32, 36, 48, 64, 72, 96]

function FontSizeField({
  value,
  autoFit,
  canAutoFit,
  disabled,
  onChange,
  onAutoFit,
}: {
  /** `null` when the characters don't share one. */
  value: number | null
  autoFit: boolean
  canAutoFit: boolean
  disabled: boolean
  onChange: (value: number) => void
  onAutoFit: () => void
}) {
  const { t } = useTranslation()
  const id = useId()
  const [draft, setDraft] = useState<number | null>(value)
  const pressed = useRef<number | null>(null)

  useEffect(() => {
    setDraft(autoFit ? null : value)
  }, [autoFit, value])

  // The stepper buttons walk the preset scale instead of the fixed step, so their changes are
  // vetoed and the preset is tracked here until the press is released and committed once.
  const change: NonNullable<ComponentProps<typeof NumberField>['onValueChange']> = (
    next,
    details,
  ) => {
    if (details.reason !== 'increment-press' && details.reason !== 'decrement-press') {
      setDraft(next)
      return
    }
    details.cancel()
    const current = pressed.current ?? draft ?? value ?? 24
    const preset =
      details.reason === 'increment-press'
        ? fontSizePresets.find((size) => size > current)
        : fontSizePresets.findLast((size) => size < current)
    if (preset !== undefined) {
      pressed.current = preset
      setDraft(preset)
    }
  }

  const select = (choice: string) => {
    if (choice === 'auto') {
      setDraft(null)
      onAutoFit()
      return
    }
    const size = Number(choice)
    if (Number.isFinite(size) && size > 0 && size <= 300) {
      setDraft(size)
      onChange(size)
    }
  }

  const commit: NonNullable<ComponentProps<typeof NumberField>['onValueCommitted']> = (
    next,
    details,
  ) => {
    if (details.reason === 'increment-press' || details.reason === 'decrement-press') {
      const preset = pressed.current
      pressed.current = null
      if (preset !== null) onChange(preset)
      return
    }
    if (next !== null && next > 0 && next <= 300) {
      onChange(next)
    } else {
      setDraft(autoFit ? null : value)
    }
  }

  return (
    <NumberField
      id={id}
      name='font-size'
      className='min-w-0'
      disabled={disabled}
      value={draft}
      min={0.5}
      max={300}
      step={0.5}
      onValueChange={change}
      onValueCommitted={commit}
    >
      <NumberFieldGroup>
        <NumberFieldDecrement aria-label={t('inspector.decreaseFontSize')}>
          <Minus />
        </NumberFieldDecrement>
        <NumberFieldInput
          data-testid='type-size'
          aria-label={t('inspector.fontSize')}
          // Enter applies the size, as in Photoshop's Character panel.
          onKeyDown={(event) => event.key === 'Enter' && event.currentTarget.blur()}
          placeholder={autoFit ? t('inspector.auto') : undefined}
          className='px-1.5 text-center text-ellipsis'
        />
        <NumberFieldIncrement aria-label={t('inspector.increaseFontSize')}>
          <Plus />
        </NumberFieldIncrement>
        <DropdownMenu>
          <DropdownMenuTrigger
            disabled={disabled}
            aria-label={t('inspector.chooseFontSize')}
            className='grid size-6 shrink-0 place-items-center border-l border-input text-muted-foreground outline-none hover:bg-muted hover:text-foreground focus-visible:bg-muted focus-visible:text-foreground disabled:pointer-events-none disabled:opacity-40'
          >
            <ChevronDown className='size-3' />
          </DropdownMenuTrigger>
          <DropdownMenuContent
            align='end'
            className='w-28 min-w-28 border border-border/50 p-0.5 shadow-sm ring-0'
          >
            <DropdownMenuRadioGroup value={autoFit ? 'auto' : String(value)} onValueChange={select}>
              {canAutoFit && (
                <DropdownMenuRadioItem value='auto' className='min-h-6 py-0.5 text-[10px]'>
                  {t('inspector.auto')}
                </DropdownMenuRadioItem>
              )}
              {fontSizePresets.map((size) => (
                <DropdownMenuRadioItem
                  key={size}
                  value={String(size)}
                  className='min-h-6 py-0.5 text-[10px] tabular-nums'
                >
                  {size}
                </DropdownMenuRadioItem>
              ))}
            </DropdownMenuRadioGroup>
          </DropdownMenuContent>
        </DropdownMenu>
      </NumberFieldGroup>
    </NumberField>
  )
}
