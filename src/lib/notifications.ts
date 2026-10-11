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

/** Notify without putting names, handles, or message text on screen. */
export async function notifyNew(items: PendingReply[]): Promise<void> {
  if (items.length === 0) return;
  if (!(await ensurePermission())) return;

  sendNotification({
    title:
      items.length === 1
        ? '1 conversation needs attention'
        : `${items.length} conversations need attention`,
    body: 'Open Reply Hardened to review.',
  });
}
