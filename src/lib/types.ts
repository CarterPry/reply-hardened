export interface PendingReply {
  chatGuid: string;
  name: string;
  handle: string;
  lastMessageGuid: string;
  /** Unix timestamp in seconds. */
  timestamp: number;
  isGroup: boolean;
  memberCount: number;
  service: string | null;
  /** For groups: ordered list of resolved participant names (or raw
   *  handle strings if no AddressBook match). Empty for 1:1. */
  participants: string[];
  /** Resolved name of the most recent sender, for groups. */
  lastSender: string | null;
  /** Preview text of the latest message (whitespace-normalized,
   *  truncated). `null` for stickers, attachment-only messages, or
   *  rows where the typedstream parse failed. */
  preview: string | null;
  /** Text to type into Messages' search when opening a group chat —
   *  the real display name, or participant first names for unnamed
   *  groups (the "+N" display label doesn't match anything). */
  searchHint: string;
  /** The thread was closed by a tapback rather than a real reply
   *  ("you reacted, never replied" / "they reacted"). Rendered
   *  demoted and excluded from badge counts, but still visible. */
  acknowledged: boolean;
}

export interface MutedChat {
  chatGuid: string;
  /** Display name captured at mute time. */
  name: string;
}

/** Which list to fetch: chats where you owe a reply, or chats where
 *  you're waiting on one (your message unanswered for ≥3 days). */
export type ReplyMode = 'owed' | 'waiting';

export type AppError =
  | { kind: 'permission_denied'; path: string }
  | { kind: 'not_found'; path: string }
  | { kind: 'database'; message: string }
  | { kind: 'io'; message: string }
  | { kind: 'other'; message: string };
