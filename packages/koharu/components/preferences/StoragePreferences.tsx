'use client'

import { relaunch } from '@tauri-apps/plugin-process'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'

import {
  PreferencePage,
  PreferenceRow,
  PreferenceSection,
} from '@/components/preferences/PreferenceFields'
import { call } from '@/lib/backend'
import { commands, type DataFolder } from '@koharu/ipc/protocol'
import { Button } from '@koharu/ui/components/button'

/** Where Koharu keeps its data, which a folder chosen here holds from the next start on. Nothing
 * moves: the old folder stays for the user to delete, and models download again. */
export function StoragePreferences() {
  const { t } = useTranslation()
  const [folder, setFolder] = useState<DataFolder | null>(null)

  useEffect(() => {
    void call(commands.getDataFolder)
      .then(setFolder)
      .catch(() => undefined)
  }, [])

  const run = (command: () => Promise<DataFolder>) =>
    void call(command)
      .then(setFolder)
      .catch(() => undefined)

  return (
    <PreferencePage
      title={t('settings.storage.title')}
      description={t('settings.storage.description')}
    >
      <PreferenceSection title={t('settings.storage.folder')}>
        <PreferenceRow
          title={t('settings.storage.inUse')}
          description={t('settings.storage.inUseDescription')}
          align='start'
        >
          <div className='flex flex-col gap-2'>
            <p
              className='truncate rounded-md border border-input px-2.5 py-1.5 font-mono text-[11px]'
              title={folder?.current}
            >
              {folder?.current ?? '…'}
            </p>
            <div className='flex gap-2'>
              <Button
                type='button'
                variant='outline'
                size='sm'
                disabled={!folder}
                onClick={() => run(commands.chooseDataFolder)}
              >
                {t('settings.storage.choose')}
              </Button>
              <Button
                type='button'
                variant='ghost'
                size='sm'
                disabled={!folder?.chosen}
                onClick={() => run(commands.resetDataFolder)}
              >
                {t('settings.storage.useDefault')}
              </Button>
            </div>
          </div>
        </PreferenceRow>
        {folder?.restart && (
          <PreferenceRow
            title={t('settings.storage.restartTitle')}
            description={t('settings.storage.restartDescription', {
              folder: folder.chosen ?? t('settings.storage.default'),
            })}
            align='start'
          >
            <Button type='button' size='sm' onClick={() => void relaunch()}>
              {t('settings.storage.restart')}
            </Button>
          </PreferenceRow>
        )}
      </PreferenceSection>
    </PreferencePage>
  )
}
