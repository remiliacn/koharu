import { QueryClientProvider } from '@tanstack/react-query'
import { render, waitFor } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import { KoharuApp } from '@/components/app/KoharuApp'
import { pagesKey, projectKey, queryClient } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import type { PageSummary, ProjectInfo } from '@koharu/bridge/protocol'

vi.mock('@/components/app/TitleBar', () => ({ TitleBar: () => null }))
vi.mock('@/components/editor/Editor', () => ({ Editor: () => null }))
vi.mock('@/components/preferences/SettingsPage', () => ({ SettingsPage: () => null }))
vi.mock('@/components/start/StartView', () => ({ StartView: () => null }))

const summary = (id: string): PageSummary => ({
  id,
  label: id,
  size: { width: 100, height: 100 },
  source_asset: null,
  layer_count: 0,
})

const seed = (activePage: string, pages: string[]) => {
  const project: ProjectInfo = {
    name: 'reused',
    revision: 2,
    active_page: activePage,
    can_undo: true,
    can_redo: false,
  }
  queryClient.setQueryData(projectKey, project)
  queryClient.setQueryData(pagesKey, pages.map(summary))
}

const renderApp = () =>
  render(
    <QueryClientProvider client={queryClient}>
      <KoharuApp />
    </QueryClientProvider>,
  )

describe('KoharuApp page selection', () => {
  it('replaces a selection of deleted pages with the active page after a new import', async () => {
    seed('new-1', ['new-1', 'new-2'])
    useKoharuStore.setState({ selectedPages: ['deleted'], selectedLayers: ['deleted-layer'] })

    renderApp()

    await waitFor(() => expect(useKoharuStore.getState().selectedPages).toEqual(['new-1']))
    expect(useKoharuStore.getState().selectedLayers).toEqual([])
  })

  it('drops deleted pages while keeping the remaining selection', async () => {
    seed('new-1', ['new-1', 'new-2'])
    useKoharuStore.setState({ selectedPages: ['deleted', 'new-2'] })

    renderApp()

    await waitFor(() => expect(useKoharuStore.getState().selectedPages).toEqual(['new-2']))
  })
})
