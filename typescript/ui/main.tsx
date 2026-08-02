import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import { AppErrorBoundary, installGlobalErrorCapture } from './components/AppErrorBoundary';
import { InteractionLayerProvider } from './hooks/useInteractionLayer';

installGlobalErrorCapture();
const applicationRoot = document.getElementById('root');
if (!applicationRoot) throw new Error('Application root is missing');
createRoot(applicationRoot).render(
  <StrictMode>
    <AppErrorBoundary>
      <InteractionLayerProvider applicationRoot={applicationRoot}>
        <App />
      </InteractionLayerProvider>
    </AppErrorBoundary>
  </StrictMode>,
);
