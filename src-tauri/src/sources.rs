//! The three message sources Reply reads, and how a source reports
//! itself when it can't be read.
//!
//! Every row the app surfaces carries its `Platform`, and every piece
//! of local state (done / snoozed / muted) is keyed by a chat key that
//! is namespaced per platform. iMessage keeps its bare `chat.guid` so
//! state written by earlier versions keeps working; the newer sources
//! prefix theirs (`wa:` / `sg:`) and therefore cannot collide with an
//! iMessage guid or with each other.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    IMessage,
    WhatsApp,
    Signal,
}

impl Platform {
    /// Short label for the UI chip.
    pub fn label(self) -> &'static str {
        match self {
            Platform::IMessage => "iMessage",
            Platform::WhatsApp => "WhatsApp",
            Platform::Signal => "Signal",
        }
    }

    /// The macOS application that owns this platform's conversations -
    /// what `open_chat` activates and drives.
    pub fn app_name(self) -> &'static str {
        match self {
            Platform::IMessage => "Messages",
            Platform::WhatsApp => "WhatsApp",
            Platform::Signal => "Signal",
        }
    }

    /// Prefix for state keys. Empty for iMessage on purpose: its keys
    /// are historical and must stay byte-identical.
    pub fn key_prefix(self) -> &'static str {
        match self {
            Platform::IMessage => "",
            Platform::WhatsApp => "wa:",
            Platform::Signal => "sg:",
        }
    }

    /// Namespaced state key for one of this platform's chats.
    pub fn chat_key(self, native_id: &str) -> String {
        format!("{}{}", self.key_prefix(), native_id)
    }

    pub const ALL: [Platform; 3] = [Platform::IMessage, Platform::WhatsApp, Platform::Signal];
}

/// Why a source contributed no rows. Only `Ready` means "the list you
/// are looking at includes this platform".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    /// Read successfully.
    Ready,
    /// The app isn't installed, or has never been signed in on this Mac.
    Absent,
    /// The database is there but macOS wouldn't let us read it - Full
    /// Disk Access, or a denied keychain prompt for Signal's key.
    Blocked,
    /// Present and permitted, but the read failed (schema drift,
    /// corruption, a locked database).
    Failed,
}

/// Per-source outcome of the most recent poll, surfaced to the UI so a
/// silent source is visibly silent instead of looking like "all clear".
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus {
    pub platform: Platform,
    pub state: SourceState,
    /// Human-readable one-liner. Never contains message content.
    pub detail: Option<String>,
}

impl SourceStatus {
    pub fn ready(platform: Platform) -> Self {
        Self {
            platform,
            state: SourceState::Ready,
            detail: None,
        }
    }

    pub fn absent(platform: Platform, detail: impl Into<String>) -> Self {
        Self {
            platform,
            state: SourceState::Absent,
            detail: Some(detail.into()),
        }
    }

    pub fn blocked(platform: Platform, detail: impl Into<String>) -> Self {
        Self {
            platform,
            state: SourceState::Blocked,
            detail: Some(detail.into()),
        }
    }

    pub fn failed(platform: Platform, detail: impl Into<String>) -> Self {
        Self {
            platform,
            state: SourceState::Failed,
            detail: Some(detail.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imessage_keys_are_unprefixed_for_backward_compat() {
        assert_eq!(
            Platform::IMessage.chat_key("iMessage;-;+14155550123"),
            "iMessage;-;+14155550123"
        );
    }

    #[test]
    fn other_platforms_namespace_their_keys() {
        assert_eq!(
            Platform::WhatsApp.chat_key("14155550123@s.whatsapp.net"),
            "wa:14155550123@s.whatsapp.net"
        );
        assert_eq!(Platform::Signal.chat_key("abc-123"), "sg:abc-123");
    }

    #[test]
    fn keys_cannot_collide_across_platforms() {
        let id = "same-id";
        let keys = [
            Platform::IMessage.chat_key(id),
            Platform::WhatsApp.chat_key(id),
            Platform::Signal.chat_key(id),
        ];
        let unique: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), 3);
    }
}
