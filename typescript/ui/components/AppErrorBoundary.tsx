import { Component } from 'react';
import type { ErrorInfo, ReactNode } from 'react';

interface Props {
  children: ReactNode;
}

interface State {
  message: string | null;
}

function remember(kind: string, value: unknown, detail = '') {
  const message = value instanceof Error ? `${value.name}: ${value.message}\n${value.stack ?? ''}` : String(value);
  try {
    localStorage.setItem('sd.last-ui-error', JSON.stringify({
      at: new Date().toISOString(),
      kind,
      message,
      detail,
    }));
  } catch {
    // The visible fallback and console entry still carry the failure if storage is unavailable.
  }
  console.error(`[SyncDash ${kind}]`, value, detail);
}

export function installGlobalErrorCapture() {
  window.addEventListener('error', (event) => {
    remember('window error', event.error ?? event.message, `${event.filename}:${event.lineno}:${event.colno}`);
  });
  window.addEventListener('unhandledrejection', (event) => {
    remember('unhandled rejection', event.reason);
  });
}

export class AppErrorBoundary extends Component<Props, State> {
  state: State = { message: null };

  static getDerivedStateFromError(error: unknown): State {
    return { message: error instanceof Error ? `${error.name}: ${error.message}` : String(error) };
  }

  componentDidCatch(error: unknown, info: ErrorInfo) {
    remember('render error', error, info.componentStack ?? '');
  }

  render() {
    if (!this.state.message) return this.props.children;
    return (
      <main className="fatal-root">
        <h1>SyncDash UI stopped rendering</h1>
        <p>{this.state.message}</p>
        <p>The error was saved as <code>sd.last-ui-error</code>. Reload the window after recording it.</p>
      </main>
    );
  }
}
