import { ProgressPage } from '#ui/pages/progress/ProgressPage.tsx';

/// Progress-window composition root. The page owns run state and lifecycle subscriptions.
export function ProgressWindow() {
  return <ProgressPage />;
}
