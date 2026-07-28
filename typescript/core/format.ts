// Framework-free on purpose: both windows and the CSV export render these same strings, so the formats
// below are an output contract rather than a rendering detail of any one screen.

export const p2 = (n: number) => String(n).padStart(2, '0');

/// Mirrors Rust `foundation::fmt::human_bytes` exactly — same units, same tiers, same decimals.
/// It has to: the CLI and this window render the same byte counts, and they previously disagreed
/// (`1.5 MiB` there, `1.5 MB` here) because the divisor was 1024 while the label said MB.
/// A loop rather than `1 << 40` for the top tier: JS bitwise operands are 32-bit, so `1 << 40` is 256.
export function humanSize(b?: number): string {
  if (b === undefined) return '';
  const U = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let v = b;
  let i = 0;
  while (v >= 1024 && i < U.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${b} B` : `${v.toFixed(1)} ${U[i]}`;
}

/// This year shows only MM-DD HH:mm; earlier years add the year — the column is narrow, density wins
export function fmtTime(ms: number): string {
  if (!ms) return '';
  const d = new Date(ms);
  const md = `${p2(d.getMonth() + 1)}-${p2(d.getDate())} ${p2(d.getHours())}:${p2(d.getMinutes())}`;
  return d.getFullYear() === new Date().getFullYear() ? md : `${d.getFullYear()}-${md}`;
}

export function relTime(ts: number): string {
  const d = Date.now() - ts;
  if (d < 60_000) return 'just now';
  if (d < 3600_000) return `${Math.floor(d / 60_000)} min ago`;
  if (d < 86400_000) return `${Math.floor(d / 3600_000)} h ago`;
  return `${Math.floor(d / 86400_000)} d ago`;
}

/// Wall-clock stamp for a log row, to the millisecond — ordering inside one run is the whole point
export function hms(ts: number): string {
  const d = new Date(ts);
  const p = (n: number, w = 2) => String(n).padStart(w, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`;
}

/// Elapsed / remaining duration: h:mm:ss once it passes an hour, m:ss below that
export function humanDuration(ms: number): string {
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), ss = s % 60;
  return h > 0 ? `${h}:${p2(m)}:${p2(ss)}` : `${m}:${p2(ss)}`;
}

// Relative paths inside a plan always use '/', regardless of the root's own separator.

export function dirOf(p: string): string {
  const i = p.lastIndexOf('/');
  return i < 0 ? '' : p.slice(0, i);
}

export function baseOf(p: string): string {
  const i = p.lastIndexOf('/');
  return i < 0 ? p : p.slice(i + 1);
}

export function sepOf(root: string): string {
  return root.includes('\\') ? '\\' : '/';
}

export function fullPath(root: string, rel: string): string {
  const sep = sepOf(root);
  const r = root.endsWith(sep) ? root.slice(0, -1) : root;
  return r + sep + (sep === '\\' ? rel.replace(/\//g, '\\') : rel);
}
