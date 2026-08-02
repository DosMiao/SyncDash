import { useEffect, type MutableRefObject } from 'react';
import { listenToMainWindowEvent } from '#core/infrastructure/tauri/mainWindow.ts';
import { autoScanStatus } from '#core/infrastructure/tauri/commands/autoscan.ts';
import type { AutoScanStatusDto } from '#core/types/generated/AutoScanStatusDto.ts';
import type { AutoScanTriggerDto } from '#core/types/generated/AutoScanTriggerDto.ts';
import type { AutoScanStatusSource } from '#core/application/autoscan/autoscan.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';

export function useAutoScanSubscriptions(options: {
  acceptStatus: (status: AutoScanStatusDto, source: AutoScanStatusSource) => boolean;
  trigger: MutableRefObject<(trigger: AutoScanTriggerDto) => Promise<void>>;
  setStatus: StatusApi['setMessage'];
}) {
  const { acceptStatus, trigger, setStatus } = options;
  useEffect(() => {
    let disposed = false;
    const removers: Array<() => void> = [];
    void (async () => {
      const installed = await Promise.allSettled([
        listenToMainWindowEvent<AutoScanStatusDto>('autoscan-status', ({ payload }) => {
          if (disposed) return;
          const accepted = acceptStatus(payload, 'event');
          if (accepted) setStatus(`AutoScan: ${payload.detail}`, payload.active ? '' : 'err');
        }),
        listenToMainWindowEvent<AutoScanTriggerDto>('autoscan-trigger', ({ payload }) => {
          if (!disposed) void trigger.current(payload);
        }),
      ]);
      for (const [index, result] of installed.entries()) {
        if (result.status === 'fulfilled') {
          if (disposed) result.value(); else removers.push(result.value);
        } else if (!disposed) {
          setStatus(
            `AutoScan ${index === 0 ? 'status' : 'trigger'} subscription failed: ${result.reason}`,
            'err',
          );
        }
      }
      if (disposed) return;
      try {
        acceptStatus(await autoScanStatus(), 'snapshot');
      } catch (error) {
        if (!disposed) setStatus(`AutoScan status is unavailable: ${error}`, 'err');
      }
    })();
    return () => {
      disposed = true;
      for (const remove of removers) remove();
    };
  }, [acceptStatus, setStatus, trigger]);
}
