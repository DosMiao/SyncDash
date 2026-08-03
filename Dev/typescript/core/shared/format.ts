const padDigits = (value: number, width = 2) => String(value).padStart(width, '0');

/// The one grouping rule for every integer count the product renders.
///
/// Pinned to en-US rather than the webview locale because the product text is en-US and because two
/// panels of the same screen used to disagree: the run-scope facets pinned en-US while the result
/// bar and the status bar followed the host locale, so the same number was spelled two ways at once.
const COUNT_FORMAT = new Intl.NumberFormat('en-US');

export function formatCount(value: number): string {
  return COUNT_FORMAT.format(value);
}

/// Byte count → `1.5 MiB`, spelled the way `foundation::fmt::human_bytes` spells it.
///
/// Rust is the arbiter, and the rule it applies at one decimal is round-half-to-even, because
/// `format!("{v:.1}")` rounds the exact binary value that way. Ties are reachable rather than
/// theoretical: `n / 1024^k` is a dyadic rational and therefore exact in f64, so the fraction can
/// land on `.25` or `.75` — the only two dyadic tie fractions. `toFixed` breaks both ties away from
/// zero, which spelled every `.25` one tenth higher than the CLI did for the very same run total.
/// The tie test multiplies by 4 and 2, which are exponent shifts and so exact; multiplying by 10
/// would introduce a rounding step into the test for the rounding.
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
  if (unitIndex === 0) return `${bytes} B`;
  const isTie = Number.isInteger(scaledBytes * 4) && !Number.isInteger(scaledBytes * 2);
  if (isTie) {
    const whole = Math.floor(scaledBytes);
    // `.25` sits between the 2 and 3 tenth, `.75` between the 7 and 8: the even tenth is 2 and 8.
    const evenTenth = scaledBytes - whole < 0.5 ? 2 : 8;
    return `${whole}.${evenTenth} ${units[unitIndex]}`;
  }
  return `${scaledBytes.toFixed(1)} ${units[unitIndex]}`;
}

export function formatFileTimestamp(timestampMs: number): string {
  if (!timestampMs) return '';
  const date = new Date(timestampMs);
  const monthDayAndTime = `${padDigits(date.getMonth() + 1)}-${padDigits(date.getDate())} ${padDigits(date.getHours())}:${padDigits(date.getMinutes())}`;
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
  return `${padDigits(date.getHours())}:${padDigits(date.getMinutes())}:${padDigits(date.getSeconds())}.${padDigits(date.getMilliseconds(), 3)}`;
}

export function humanDuration(ms: number): string {
  const totalSeconds = Math.floor(ms / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  return hours > 0
    ? `${hours}:${padDigits(minutes)}:${padDigits(seconds)}`
    : `${minutes}:${padDigits(seconds)}`;
}

/// Throughput in the same binary unit family as `humanSize`, for every window.
///
/// Two decimals, deliberately: the Compare panel used one, but it only ever renders rates above its
/// own display floor, while the progress window shows the real instantaneous rate, and one decimal
/// spells every transfer slower than 50 KiB/s as `0.0 MiB/s`. Two decimals is the rule that neither
/// surface has to round a live rate away.
///
/// Formatting only. Whether a rate is worth showing at all is a per-surface decision — the Compare
/// stage row suppresses its own sub-512-KiB/s rates so a compact row does not flicker — and putting
/// that floor here would silence the progress window's slow-transfer readout.
export function humanByteRate(bytesPerSecond: number): string {
  return `${(bytesPerSecond / (1 << 20)).toFixed(2)} MiB/s`;
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

/// Root phrase + portable relative path → the path spelled the way the *far* machine spells it.
///
/// Mirrors Rust `foundation::path::join_display`, which is the arbiter. Every trailing separator is
/// trimmed, of either flavour — not just one of the inferred flavour. A root recorded as
/// `//server/share//` or `C:\root\\` is ordinary for UNC phrases and hand-edited job files, and
/// trimming only the last one emitted a doubled separator here while the CSV export emitted a
/// single one, for the same row.
export function joinDisplayPath(root: string, relativePath: string): string {
  const separator = displayPathSeparator(root);
  const normalizedRoot = root.replace(/[/\\]+$/, '');
  const nativeRelativePath = separator === '\\' ? relativePath.replace(/\//g, '\\') : relativePath;
  return normalizedRoot + separator + nativeRelativePath;
}
