import { WorkspacePage } from '#ui/pages/workspace/WorkspacePage.tsx';

/// Main-window composition root. Window bootstrap stays independent from workspace behavior.
export function MainWindow() {
  return <WorkspacePage />;
}
