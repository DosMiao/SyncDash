import { useEffect, type Dispatch, type SetStateAction } from 'react';
import { getAppVersion } from '#core/infrastructure/tauri/mainWindow.ts';
import { jobsDir } from '#core/infrastructure/tauri/commands/main.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';

export function useWorkspaceBootstrap(options: {
  refreshJobs: () => Promise<JobDto[]>;
  refreshLatestRunSummaries: () => void;
  setJobsDir: Dispatch<SetStateAction<string>>;
  setAppVersion: Dispatch<SetStateAction<string>>;
  setStatus: StatusApi['setMessage'];
}) {
  const { refreshJobs, refreshLatestRunSummaries, setJobsDir, setAppVersion, setStatus } = options;
  useEffect(() => {
    void (async () => {
      try {
        const list = await refreshJobs();
        refreshLatestRunSummaries();
        setJobsDir(await jobsDir());
        let versionError: unknown = null;
        try {
          setAppVersion(`v${await getAppVersion()}`);
        } catch (error) {
          versionError = error;
          setAppVersion('version unavailable');
        }
        if (versionError) {
          setStatus(`Initialized, but the application version could not be read: ${versionError}`, 'err');
        } else {
          setStatus(list.length
            ? 'Select a job on the left to start'
            : 'No jobs — drop a <name>.toml into the jobs directory');
        }
      } catch (error) {
        setStatus(`Init failed: ${error}`, 'err');
      }
    })();
  }, [refreshJobs, refreshLatestRunSummaries, setAppVersion, setJobsDir, setStatus]);
}
