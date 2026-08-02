import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow, ProgressBarStatus } from '@tauri-apps/api/window';

export const listenToProgressWindowEvent = listen;
export const getProgressWindow = getCurrentWindow;
export { ProgressBarStatus };
