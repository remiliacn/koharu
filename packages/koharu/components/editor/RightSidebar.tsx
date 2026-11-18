'use client'

import { Bot, SlidersHorizontal, Type } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'

import { AgentPanel } from '@/components/editor/AgentPanel'
import { LayersPanel } from '@/components/editor/LayersPanel'
import { TypePanel } from '@/components/editor/TypePanel'
import { useProject } from '@/lib/queries'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@koharu/ui/components/tabs'

/** The right pane: Properties, the selected texts' type over the page's layers, and Agent, as
 * tabs. The agent's tab stays mounted, so its conversation outlives a switch, and starts over with
 * another project, as the desktop's conversation does. */
export function RightSidebar() {
  const { t } = useTranslation()
  const project = useProject().data?.name ?? null
  const [panel, setPanel] = useState<'properties' | 'agent'>('properties')
  // The shown tab in the accent colour, hovered or not, its underline too.
  const tab =
    'h-7 flex-none gap-1 px-1.5 text-[10px] font-semibold after:bg-primary data-active:text-primary data-active:hover:text-primary [&_svg]:size-3'
  return (
    <aside className='flex h-full min-h-0 flex-col bg-[var(--surface-panel)]'>
      <Tabs
        value={panel}
        onValueChange={(next) => setPanel(next as 'properties' | 'agent')}
        className='min-h-0 flex-1 gap-0'
      >
        <TabsList
          variant='line'
          className='h-10 w-full shrink-0 justify-start rounded-none border-b border-border/80 px-1'
        >
          <TabsTrigger value='properties' className={tab}>
            <SlidersHorizontal /> {t('agent.properties')}
          </TabsTrigger>
          <TabsTrigger value='agent' className={tab}>
            <Bot /> {t('agent.title')}
          </TabsTrigger>
        </TabsList>
        <TabsContent value='properties' className='flex min-h-0 flex-col'>
          <header className='flex h-8 shrink-0 items-center gap-1.5 border-b border-border/80 px-2'>
            <Type className='size-3 text-primary' />
            <h2 className='text-[10px] font-semibold'>{t('inspector.type')}</h2>
          </header>
          <section className='min-w-0 shrink-0 border-b border-border/80'>
            <TypePanel />
          </section>
          <LayersPanel />
        </TabsContent>
        <TabsContent value='agent' keepMounted className='flex min-h-0 flex-col'>
          <AgentPanel key={project} />
        </TabsContent>
      </Tabs>
    </aside>
  )
}
