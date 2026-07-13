import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from '@tauri-apps/plugin-notification';
import type { PendingReply } from './types';

let permissionState: 'unknown' | 'granted' | 'denied' = 'unknown';

async function ensurePermission(): Promise<boolean> {
  if (permissionState === 'granted') return true;
  if (permissionState === 'denied') return false;
  const granted = await isPermissionGranted();
  if (granted) {
    permissionState = 'granted';
    return true;
  }
  const requested = await requestPermission();
  if (requested === 'granted') {
    permissionState = 'granted';
    return true;
  }
  permissionState = 'denied';
  return false;
}

/** Fire a single notification summarizing the new pendings. */
export async function notifyNew(items: PendingReply[]): Promise<void> {
  if (items.length === 0) return;
  if (!(await ensurePermission())) return;

  if (items.length === 1) {
    const item = items[0]!;
    sendNotification({
      title: item.name,
      body: item.preview ?? 'New message, needs reply',
    });
    return;
  }

  const names = items
    .slice(0, 3)
    .map((i) => i.name)
    .join(', ');
  const extra = items.length > 3 ? ` +${items.length - 3} more` : '';
  sendNotification({
    title: `${items.length} new messages`,
    body: `${names}${extra}`,
  });
}
