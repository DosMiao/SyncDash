import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog';
import type { OpenDialogOptions, SaveDialogOptions } from '@tauri-apps/plugin-dialog';

export type DirectoryPickerOptions = Omit<OpenDialogOptions, 'directory' | 'multiple'>;

export const pickDirectory = (options: DirectoryPickerOptions): Promise<string | null> => (
  openDialog({ ...options, directory: true, multiple: false })
);

export const pickSavePath = (options: SaveDialogOptions): Promise<string | null> => saveDialog(options);
