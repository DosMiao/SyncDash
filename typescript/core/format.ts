const padTwoDigits = (value: number) => String(value).padStart(2, '0');

/// Must remain identical to `foundation::fmt::human_bytes`: binary units and one decimal above bytes.
/// Iteration avoids JavaScript's 32-bit bitwise-operand limit for TiB values.
export function humanSize(bytes?: number): string {
  if (bytes === undefined) return '';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let scaledBytes = bytes;
  let unitIndex = 0;
  while (scaledBytes >= 1024 && unitIndex < units.length - 1) {
    scaledBytes /= 1024;
    unitIndex++;
  }
  return unitIndex === 0 ? `${bytes} B` : `${scaledBytes.toFixed(1)} ${units[unitIndex]}`;
}

export function formatFileTimestamp(timestampMs: number): string {
  if (!timestampMs) return '';
  const date = new Date(timestampMs);
  const monthDayAndTime = `${padTwoDigits(date.getMonth() + 1)}-${padTwoDigits(date.getDate())} ${padTwoDigits(date.getHours())}:${padTwoDigits(date.getMinutes())}`;
  return date.getFullYear() === new Date().getFullYear()
    ? monthDayAndTime
    : `${date.getFullYear()}-${monthDayAndTime}`;
}

export function formatRelativeTimestamp(timestampMs: number): string {
  const elapsedMs = Date.now() - timestampMs;
  if (elapsedMs < 60_000) return 'just now';
  if (elapsedMs < 3_600_000) return `${Math.floor(elapsedMs / 60_000)} min ago`;
  if (elapsedMs < 86_400_000) return `${Math.floor(elapsedMs / 3_600_000)} h ago`;
  return `${Math.floor(elapsedMs / 86_400_000)} d ago`;
}

export function formatLogTimestamp(timestampMs: number): string {
  const date = new Date(timestampMs);
  const pad = (value: number, width = 2) => String(value).padStart(width, '0');
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}.${pad(date.getMilliseconds(), 3)}`;
}

export function humanDuration(ms: number): string {
  const totalSeconds = Math.floor(ms / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  return hours > 0
    ? `${hours}:${padTwoDigits(minutes)}:${padTwoDigits(seconds)}`
    : `${minutes}:${padTwoDigits(seconds)}`;
}

export function parentRelativePath(relativePath: string): string {
  const separatorIndex = relativePath.lastIndexOf('/');
  return separatorIndex < 0 ? '' : relativePath.slice(0, separatorIndex);
}

export function relativePathBaseName(relativePath: string): string {
  const separatorIndex = relativePath.lastIndexOf('/');
  return separatorIndex < 0 ? relativePath : relativePath.slice(separatorIndex + 1);
}

function displayPathSeparator(root: string): string {
  return root.includes('\\') ? '\\' : '/';
}

export function joinDisplayPath(root: string, relativePath: string): string {
  const separator = displayPathSeparator(root);
  const normalizedRoot = root.endsWith(separator) ? root.slice(0, -1) : root;
  const nativeRelativePath = separator === '\\' ? relativePath.replace(/\//g, '\\') : relativePath;
  return normalizedRoot + separator + nativeRelativePath;
}
