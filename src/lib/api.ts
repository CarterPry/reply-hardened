import { invoke } from '@tauri-apps/api/core';
import type { AppError, PendingReply } from './types';

export function getPendingReplies(): Promise<PendingReply[]> {
  return invoke<PendingReply[]>('get_pending_replies');
}

export function refresh(): Promise<PendingReply[]> {
  return invoke<PendingReply[]>('refresh');
}

export function markComplete(lastMessageGuid: string): Promise<void> {
  return invoke<void>('mark_complete', { lastMessageGuid });
}

export function openFdaSettings(): Promise<void> {
  return invoke<void>('open_fda_settings');
}

export function openChat(
  handle: string,
  isGroup: boolean,
  chatGuid: string,
  name: string,
): Promise<void> {
  return invoke<void>('open_chat', { handle, isGroup, chatGuid, name });
}

export function isAppError(e: unknown): e is AppError {
  return (
    typeof e === 'object' &&
    e !== null &&
    'kind' in e &&
    typeof (e as { kind: unknown }).kind === 'string'
  );
}
