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
  /** Text to type into Messages' search when opening a group chat -
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

/** One "mark done" target: the chat plus the date (unix seconds) of
 *  its last real message when the user clicked done. The thread stays
 *  hidden until a real message with a later date arrives. */
export interface DoneItem {
  chatGuid: string;
  messageDate: number;
}

/** What `undo_last` reversed: the action kind and how many chats it
 *  touched (a bulk action is one entry). */
export interface UndoOutcome {
  action: 'done' | 'snooze' | 'mute' | string;
  count: number;
}

/** A 1:1 thread from someone with no matching contact record. */
export interface UnknownSender {
  chatGuid: string;
  /** Raw handle: phone number or email. */
  handle: string;
  service: string | null;
  /** Unix seconds of the first and most recent real message. */
  firstDate: number;
  lastDate: number;
  /** Count of real (non-tapback, non-system) messages. */
  messageCount: number;
  preview: string | null;
}

/** Which list to fetch: chats where you owe a reply, or chats where
 *  you're waiting on one (your message unanswered for ≥3 days). */
export type ReplyMode = 'owed' | 'waiting';

/** The three tabs: the two reply lists plus unknown senders. */
export type ViewMode = ReplyMode | 'unknown';

export type AppError =
  | { kind: 'permission_denied'; path: string }
  | { kind: 'not_found'; path: string }
  | { kind: 'database'; message: string }
  | { kind: 'io'; message: string }
  | { kind: 'other'; message: string };
