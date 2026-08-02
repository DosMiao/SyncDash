import type { ComponentProps, ReactNode } from 'react';
import { CompareActivityNotice, CompareCandidateNotice, CompareExecutionNotice } from '#ui/features/compare-workspace/components/CompareWorkspaceNotices.tsx';
import { ResultBar } from '#ui/features/compare-results/components/ResultBar.tsx';
import { PathLine } from '#ui/features/roots/components/PathLine.tsx';
import { Sidebar } from '../components/Sidebar.tsx';
import { StatusBar } from '../components/StatusBar.tsx';
import { Toolbar } from '../components/Toolbar.tsx';
import { WorkspaceResultsSection } from '../components/WorkspaceResultsSection.tsx';

export interface WorkspaceMainViewProps {
  sidebar: ComponentProps<typeof Sidebar>;
  toolbar: ComponentProps<typeof Toolbar>;
  pathLine: ComponentProps<typeof PathLine>;
  candidateNotice: ComponentProps<typeof CompareCandidateNotice> | null;
  activityNotice: ComponentProps<typeof CompareActivityNotice> | null;
  executionNotice: ComponentProps<typeof CompareExecutionNotice> | null;
  resultBar: ComponentProps<typeof ResultBar> | null;
  results: ComponentProps<typeof WorkspaceResultsSection>;
  logPanel: ReactNode;
  statusBar: ComponentProps<typeof StatusBar>;
}

export function WorkspaceMainView({
  sidebar,
  toolbar,
  pathLine,
  candidateNotice,
  activityNotice,
  executionNotice,
  resultBar,
  results,
  logPanel,
  statusBar,
}: WorkspaceMainViewProps) {
  return (
    <div className="app">
      <Sidebar {...sidebar} />
      <main className="main">
        <Toolbar {...toolbar} />
        <PathLine {...pathLine} />
        {candidateNotice && <CompareCandidateNotice {...candidateNotice} />}
        {activityNotice && <CompareActivityNotice {...activityNotice} />}
        {executionNotice && <CompareExecutionNotice {...executionNotice} />}
        {resultBar && <ResultBar {...resultBar} />}
        <WorkspaceResultsSection {...results} />
        {logPanel}
        <StatusBar {...statusBar} />
      </main>
    </div>
  );
}
