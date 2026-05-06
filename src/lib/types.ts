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
}

export type AppError =
  | { kind: 'permission_denied'; path: string }
  | { kind: 'not_found'; path: string }
  | { kind: 'database'; message: string }
  | { kind: 'io'; message: string }
  | { kind: 'other'; message: string };
