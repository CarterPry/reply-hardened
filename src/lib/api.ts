import { invoke } from '@tauri-apps/api/core';
import type { AppError, MutedChat, PendingReply, ReplyMode } from './types';

export function getReplies(mode: ReplyMode): Promise<PendingReply[]> {
  return invoke<PendingReply[]>('get_replies', { mode });
}

export function markComplete(lastMessageGuid: string): Promise<void> {
  return invoke<void>('mark_complete', { lastMessageGuid });
}

/** Hide a chat from both lists until `until` (unix seconds). */
export function snoozeChat(chatGuid: string, until: number): Promise<void> {
  return invoke<void>('snooze_chat', { chatGuid, until });
}

export function muteChat(chatGuid: string, name: string): Promise<void> {
  return invoke<void>('mute_chat', { chatGuid, name });
}

export function unmuteChat(chatGuid: string): Promise<void> {
  return invoke<void>('unmute_chat', { chatGuid });
}

export function listMuted(): Promise<MutedChat[]> {
  return invoke<MutedChat[]>('list_muted');
}

export function openFdaSettings(): Promise<void> {
  return invoke<void>('open_fda_settings');
}

export function openAccessibilitySettings(): Promise<void> {
  return invoke<void>('open_accessibility_settings');
}

export function openAutomationSettings(): Promise<void> {
  return invoke<void>('open_automation_settings');
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
