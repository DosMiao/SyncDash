import { Component } from 'react';
import type { ErrorInfo, ReactNode } from 'react';

interface AppErrorBoundaryProps {
  children: ReactNode;
}

interface AppErrorBoundaryState {
  message: string | null;
  diagnosticStorage: 'pending' | 'saved' | { error: string };
}

function remember(kind: string, value: unknown, detail = ''): string | null {
  const message = value instanceof Error ? `${value.name}: ${value.message}\n${value.stack ?? ''}` : String(value);
  let storageFailure: string | null = null;
  try {
    localStorage.setItem('sd.last-ui-error', JSON.stringify({
      at: new Date().toISOString(),
      kind,
      message,
      detail,
    }));
  } catch (storageError) {
    storageFailure = String(storageError);
    console.error('[SyncDash diagnostic storage]', storageError);
  }
  console.error(`[SyncDash ${kind}]`, value, detail);
  return storageFailure;
}

export function installGlobalErrorCapture() {
  window.addEventListener('error', (event) => {
    remember('window error', event.error ?? event.message, `${event.filename}:${event.lineno}:${event.colno}`);
  });
  window.addEventListener('unhandledrejection', (event) => {
    remember('unhandled rejection', event.reason);
  });
}

export class AppErrorBoundary extends Component<AppErrorBoundaryProps, AppErrorBoundaryState> {
  state: AppErrorBoundaryState = { message: null, diagnosticStorage: 'pending' };

  static getDerivedStateFromError(error: unknown): AppErrorBoundaryState {
    return {
      message: error instanceof Error ? `${error.name}: ${error.message}` : String(error),
      diagnosticStorage: 'pending',
    };
  }

  componentDidCatch(error: unknown, info: ErrorInfo) {
    const storageError = remember('render error', error, info.componentStack ?? '');
    this.setState({ diagnosticStorage: storageError ? { error: storageError } : 'saved' });
  }

  render() {
    if (!this.state.message) return this.props.children;
    return (
      <main className="fatal-root">
        <h1>SyncDash UI stopped rendering</h1>
        <p>{this.state.message}</p>
        {this.state.diagnosticStorage === 'pending'
          ? <p>Saving a local diagnostic…</p>
          : this.state.diagnosticStorage === 'saved'
            ? <p>The error was saved as <code>sd.last-ui-error</code>. Reload the window after recording it.</p>
            : <p>The diagnostic could not be saved locally: {this.state.diagnosticStorage.error}. Record this screen before reloading.</p>}
      </main>
    );
  }
}
