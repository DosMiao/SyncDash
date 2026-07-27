import { createRoot } from 'react-dom/client';
import { ProgressApp } from './ProgressApp';

// No StrictMode here: the run-progress listener and the 100 ms redraw tick are the whole point of this
// window, and StrictMode's deliberate double-mount would register both twice in development.
createRoot(document.getElementById('root')!).render(<ProgressApp />);
