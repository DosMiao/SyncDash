import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { MainWindow } from './MainWindow.tsx';
import { AppErrorBoundary, installGlobalErrorCapture } from '#ui/shared/errors/AppErrorBoundary.tsx';
import { InteractionLayerProvider } from '#ui/shared/interaction/useInteractionLayer.tsx';

installGlobalErrorCapture();
const applicationRoot = document.getElementById('root');
if (!applicationRoot) throw new Error('Application root is missing');
createRoot(applicationRoot).render(
  <StrictMode>
    <AppErrorBoundary>
      <InteractionLayerProvider applicationRoot={applicationRoot}>
        <MainWindow />
      </InteractionLayerProvider>
    </AppErrorBoundary>
  </StrictMode>,
);
