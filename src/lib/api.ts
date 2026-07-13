import { invoke } from '@tauri-apps/api/core';
import type {
  AppError,
  DoneItem,
  MutedChat,
  PendingReply,
  ReplyMode,
  UndoOutcome,
  UnknownSender,
} from './types';

export function getReplies(mode: ReplyMode): Promise<PendingReply[]> {
  return invoke<PendingReply[]>('get_replies', { mode });
}

export function getUnknownSenders(): Promise<UnknownSender[]> {
  return invoke<UnknownSender[]>('get_unknown_senders');
}

/** Mark chats done. Always a batch (single row = batch of one) so a
 *  bulk action lands as ONE undo entry. */
export function markDone(items: DoneItem[]): Promise<void> {
  return invoke<void>('mark_done', { items });
}

/** Hide chats from all lists until `until` (unix seconds). */
export function snoozeChats(chatGuids: string[], until: number): Promise<void> {
  return invoke<void>('snooze_chats', { chatGuids, until });
}

export function muteChats(chats: MutedChat[]): Promise<void> {
  return invoke<void>('mute_chats', { chats });
}

/** Reverse the most recent triage action; null if nothing to undo. */
export function undoLast(): Promise<UndoOutcome | null> {
  return invoke<UndoOutcome | null>('undo_last');
}

/** Open the Contacts add flow prefilled with this handle (via vCard). */
export function addToContacts(handle: string): Promise<void> {
  return invoke<void>('add_to_contacts', { handle });
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
