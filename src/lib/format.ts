/** Compact relative time, e.g. "2d ago", "3h ago", "just now". */
export function formatRelative(unixSeconds: number, nowMs: number = Date.now()): string {
  const diff = nowMs / 1000 - unixSeconds;
  if (diff < 45) return 'just now';
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  if (diff < 86400 * 7) return `${Math.floor(diff / 86400)}d ago`;
  if (diff < 86400 * 30) return `${Math.floor(diff / (86400 * 7))}w ago`;
  if (diff < 86400 * 365) return `${Math.floor(diff / (86400 * 30))}mo ago`;
  return `${Math.floor(diff / (86400 * 365))}y ago`;
}

/** How overdue a thread is. Drives the timestamp color: fresh is
 *  neutral, aging (≥3 days) warns, stale (≥7 days) alerts. */
export type AgeTier = 'fresh' | 'aging' | 'stale';

export function ageTier(unixSeconds: number, nowMs: number = Date.now()): AgeTier {
  const days = (nowMs / 1000 - unixSeconds) / 86400;
  if (days >= 7) return 'stale';
  if (days >= 3) return 'aging';
  return 'fresh';
}

/** Unix seconds for the next 9am local time - the snooze target. */
export function nextMorning(nowMs: number = Date.now()): number {
  const d = new Date(nowMs);
  d.setHours(9, 0, 0, 0);
  if (d.getTime() <= nowMs) d.setDate(d.getDate() + 1);
  return Math.floor(d.getTime() / 1000);
}

export function initials(name: string): string {
  const trimmed = name.trim();
  if (!trimmed) return '?';
  const parts = trimmed.split(/\s+/);
  if (parts.length === 1) return parts[0]!.slice(0, 1).toUpperCase();
  return (parts[0]![0]! + parts[parts.length - 1]![0]!).toUpperCase();
}
