//! Every behavioral threshold and heuristic toggle in one place.
//!
//! These are compile-time defaults. If you disagree with one, change
//! it here and rebuild — nothing else reads from disk or env.

/// Hide rows whose last message is older than this. Anything past
/// three months is effectively dead — surfacing it creates noise,
/// not signal. Default: 90 days.
pub const STALE_AFTER_SECS: i64 = 90 * 86_400;

/// In "waiting on them" mode, only surface threads where your
/// unanswered message has sat for at least this long. A text sent an
/// hour ago isn't a follow-up candidate yet. Default: 3 days.
pub const WAITING_MIN_AGE_SECS: i64 = 3 * 86_400;

/// Cap the preview string at this many characters before sending to
/// the frontend. The UI truncates visually too, but bounding payload
/// size keeps things sane for messages with very long attachments.
pub const PREVIEW_CHARS: usize = 140;

/// When the last substantive message contains a question mark, a
/// tapback on it does NOT acknowledge the thread — someone asked a
/// question, and a thumbs-up is not an answer. The row stays fully
/// open. Default: on.
pub const QUESTION_MARK_KEEPS_OPEN: bool = true;

/// What an acknowledging tapback does to a row. `true` (default):
/// the row stays visible but demoted — dimmed, sorted last, excluded
/// from badge counts ("they reacted" / "you reacted, never replied").
/// `false`: legacy behavior, the row is cleared from the list
/// entirely.
pub const TAPBACK_ACK_DEMOTES: bool = true;

/// Drop 1:1 threads whose handle is a bare 5–6 digit short code
/// (marketing/OTP senders like 22395). You never owe a short code a
/// reply and you're never waiting on one. Group chats and real phone
/// numbers/emails are unaffected. Default: on.
pub const FILTER_SHORT_CODES: bool = true;

/// Digit-count range treated as a short code by `FILTER_SHORT_CODES`.
/// US short codes are 5–6 digits; real numbers are 10+.
pub const SHORT_CODE_DIGITS_MIN: usize = 5;
pub const SHORT_CODE_DIGITS_MAX: usize = 6;
