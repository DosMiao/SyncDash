import { getVersion } from '@tauri-apps/api/app';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWebview } from '@tauri-apps/api/webview';

export const getAppVersion = getVersion;
export const listenToMainWindowEvent = listen;
export const getMainWebview = getCurrentWebview;
