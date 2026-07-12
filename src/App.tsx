import { useCallback, useEffect, useRef, useState } from 'react';
import { Row } from './components/Row';
import { PermissionPrompt } from './components/PermissionPrompt';
import {
  getReplies,
  isAppError,
  listMuted,
  markComplete,
  muteChat,
  openAccessibilitySettings,
  openAutomationSettings,
  openChat,
  openFdaSettings,
  snoozeChat,
  unmuteChat,
} from './lib/api';
import { ageTier, formatRelative, nextMorning } from './lib/format';
import { notifyNew } from './lib/notifications';
import type { AppError, MutedChat, PendingReply, ReplyMode } from './lib/types';

const POLL_MS = 30_000;

interface Lists {
  owed: PendingReply[];
  waiting: PendingReply[];
}

/** Which permission the group-chat opener was denied, if any. */
type PermissionBanner = 'accessibility' | 'automation' | null;

export function App() {
  const [mode, setMode] = useState<ReplyMode>('owed');
  const [lists, setLists] = useState<Lists>({ owed: [], waiting: [] });
  const [muted, setMuted] = useState<MutedChat[]>([]);
  const [showMuted, setShowMuted] = useState(false);
  const [banner, setBanner] = useState<PermissionBanner>(null);
  const [error, setError] = useState<AppError | null>(null);
  const [loading, setLoading] = useState(true);
  const [removing, setRemoving] = useState<Set<string>>(new Set());
  /** Tracks the set of lastMessageGuids we've already shown to the user.
   *  Stays null until the first successful load — so the very first
   *  result doesn't trigger a flood of notifications. Only the owed
   *  list notifies; "waiting" threads are by definition not news. */
  const seenGuidsRef = useRef<Set<string> | null>(null);
  /** Monotonic fetch counter. Poll, focus, and visibility refreshes
   *  overlap; only the newest in-flight fetch may apply its results,
   *  otherwise a slow stale response lands last and the list flickers
   *  backwards. */
  const fetchSeqRef = useRef(0);

  const applyResults = useCallback(
    (owed: PendingReply[], waiting: PendingReply[], mutedList: MutedChat[]) => {
      setLists({ owed, waiting });
      setMuted(mutedList);
      setError(null);
      const nextSeen = new Set(owed.map((x) => x.lastMessageGuid));
      const prevSeen = seenGuidsRef.current;
      if (prevSeen) {
        const fresh = owed.filter((x) => !prevSeen.has(x.lastMessageGuid));
        if (fresh.length > 0) {
          notifyNew(fresh).catch((e) => console.error('notify failed', e));
        }
      }
      seenGuidsRef.current = nextSeen;
    },
    [],
  );

  const handleError = useCallback((e: unknown) => {
    setError(isAppError(e) ? e : { kind: 'other', message: String(e) });
  }, []);

  const refreshAll = useCallback(async () => {
    const seq = ++fetchSeqRef.current;
    try {
      const [owed, waiting, mutedList] = await Promise.all([
        getReplies('owed'),
        getReplies('waiting'),
        listMuted(),
      ]);
      if (seq === fetchSeqRef.current) applyResults(owed, waiting, mutedList);
    } catch (e) {
      if (seq === fetchSeqRef.current) handleError(e);
    }
  }, [applyResults, handleError]);

  const retry = useCallback(async () => {
    setLoading(true);
    await refreshAll();
    setLoading(false);
  }, [refreshAll]);

  useEffect(() => {
    let active = true;
    const seq = ++fetchSeqRef.current;
    Promise.all([getReplies('owed'), getReplies('waiting'), listMuted()])
      .then(([owed, waiting, mutedList]) => {
        if (active && seq === fetchSeqRef.current) {
          applyResults(owed, waiting, mutedList);
        }
      })
      .catch((e) => {
        if (active && seq === fetchSeqRef.current) handleError(e);
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, [applyResults, handleError]);

  // Refresh when the window regains focus or the document becomes visible.
  useEffect(() => {
    const onActivate = () => {
      if (document.visibilityState === 'visible') {
        refreshAll();
      }
    };
    window.addEventListener('focus', onActivate);
    document.addEventListener('visibilitychange', onActivate);
    return () => {
      window.removeEventListener('focus', onActivate);
      document.removeEventListener('visibilitychange', onActivate);
    };
  }, [refreshAll]);

  // Poll every 30s. The window is usually hidden (menu bar app), but
  // the poll must still run so the tray count stays current; it's a
  // cheap read-only sqlite query.
  useEffect(() => {
    const id = window.setInterval(() => {
      refreshAll();
    }, POLL_MS);
    return () => window.clearInterval(id);
  }, [refreshAll]);

  /** Run `action`, then animate the row out of both lists. */
  const dismiss = useCallback(
    async (item: PendingReply, action: () => Promise<void>) => {
      setRemoving((prev) => new Set(prev).add(item.lastMessageGuid));
      try {
        await action();
      } catch (e) {
        console.error('dismiss failed', e);
        setRemoving((prev) => {
          const next = new Set(prev);
          next.delete(item.lastMessageGuid);
          return next;
        });
        return;
      }
      // Tell the seen-set we no longer have this one so a future
      // revival (a new guid) is correctly detected as "new".
      seenGuidsRef.current?.delete(item.lastMessageGuid);
      setTimeout(() => {
        setLists((prev) => ({
          owed: prev.owed.filter((x) => x.lastMessageGuid !== item.lastMessageGuid),
          waiting: prev.waiting.filter((x) => x.lastMessageGuid !== item.lastMessageGuid),
        }));
        setRemoving((prev) => {
          const next = new Set(prev);
          next.delete(item.lastMessageGuid);
          return next;
        });
      }, 340);
    },
    [],
  );

  const onComplete = useCallback(
    (item: PendingReply) => dismiss(item, () => markComplete(item.lastMessageGuid)),
    [dismiss],
  );

  const onSnooze = useCallback(
    (item: PendingReply) => dismiss(item, () => snoozeChat(item.chatGuid, nextMorning())),
    [dismiss],
  );

  const onMute = useCallback(
    (item: PendingReply) =>
      dismiss(item, async () => {
        await muteChat(item.chatGuid, item.name);
        setMuted(await listMuted());
      }),
    [dismiss],
  );

  const onUnmute = useCallback(
    async (chat: MutedChat) => {
      try {
        await unmuteChat(chat.chatGuid);
        await refreshAll();
      } catch (e) {
        console.error('unmute failed', e);
      }
    },
    [refreshAll],
  );

  const onOpen = useCallback((item: PendingReply) => {
    openChat(item.handle, item.isGroup, item.chatGuid, item.searchHint).catch((e) => {
      const msg = String(e);
      if (msg.includes('accessibility_denied')) {
        setBanner('accessibility');
      } else if (msg.includes('automation_denied')) {
        setBanner('automation');
      } else {
        console.error('open_chat failed', e);
      }
    });
  }, []);

  const items = lists[mode];

  return (
    <div className="container">
      <header className="header">
        <h1>Replies</h1>
        <nav className="tabs" aria-label="Reply lists">
          <TabButton
            label="Owe reply"
            count={lists.owed.length}
            active={mode === 'owed'}
            onClick={() => setMode('owed')}
          />
          <TabButton
            label="Waiting"
            count={lists.waiting.length}
            active={mode === 'waiting'}
            onClick={() => setMode('waiting')}
          />
        </nav>
      </header>
      {banner && (
        <div className="banner">
          <p>
            {banner === 'accessibility'
              ? 'Opening group chats needs the Accessibility permission. Enable "iMessage Reply Tracker" under System Settings → Privacy & Security → Accessibility, then try again.'
              : 'Opening group chats needs permission to control Messages. Allow "iMessage Reply Tracker" under System Settings → Privacy & Security → Automation, then try again.'}
          </p>
          <div className="banner__actions">
            <button
              className="btn btn--primary"
              type="button"
              onClick={() => {
                const open =
                  banner === 'accessibility'
                    ? openAccessibilitySettings
                    : openAutomationSettings;
                open().catch((e) => console.error('open settings failed', e));
              }}
            >
              Open Settings
            </button>
            <button className="btn" type="button" onClick={() => setBanner(null)}>
              Dismiss
            </button>
          </div>
        </div>
      )}
      <main className="main">
        {loading ? (
          <div className="empty"><p>Loading…</p></div>
        ) : error?.kind === 'permission_denied' ? (
          <PermissionPrompt
            path={error.path}
            onGrant={() => {
              openFdaSettings().catch((e) => console.error('open settings failed', e));
            }}
            onRetry={retry}
          />
        ) : error ? (
          <ErrorBox error={error} onRetry={retry} />
        ) : items.length === 0 ? (
          mode === 'owed' ? (
            <div className="empty">
              <div className="empty__title">All clear</div>
              <p>No conversations are waiting on you.</p>
            </div>
          ) : (
            <div className="empty">
              <div className="empty__title">Nothing outstanding</div>
              <p>No unanswered messages older than 3 days.</p>
            </div>
          )
        ) : (
          <ul className="list">
            {items.map((item) => (
              <Row
                key={item.lastMessageGuid}
                item={item}
                relative={formatRelative(item.timestamp)}
                tier={ageTier(item.timestamp)}
                removing={removing.has(item.lastMessageGuid)}
                onComplete={() => onComplete(item)}
                onSnooze={() => onSnooze(item)}
                onMute={() => onMute(item)}
                onOpen={() => onOpen(item)}
              />
            ))}
          </ul>
        )}
        {!loading && !error && muted.length > 0 && (
          <footer className="muted-footer">
            <button
              className="muted-footer__toggle"
              type="button"
              onClick={() => setShowMuted((v) => !v)}
            >
              {muted.length} muted {showMuted ? '· Hide' : '· Show'}
            </button>
            {showMuted && (
              <ul className="muted-list">
                {muted.map((chat) => (
                  <li key={chat.chatGuid} className="muted-list__item">
                    <span className="muted-list__name">{chat.name}</span>
                    <button
                      className="btn btn--small"
                      type="button"
                      onClick={() => onUnmute(chat)}
                    >
                      Unmute
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </footer>
        )}
      </main>
    </div>
  );
}

function TabButton({
  label,
  count,
  active,
  onClick,
}: {
  label: string;
  count: number;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      className={`tab${active ? ' tab--active' : ''}`}
      type="button"
      aria-pressed={active}
      onClick={onClick}
    >
      {label}
      {count > 0 && <span className="tab__count">{count}</span>}
    </button>
  );
}

function ErrorBox({ error, onRetry }: { error: AppError; onRetry: () => void }) {
  const title =
    error.kind === 'not_found'
      ? 'iMessage database not found'
      : error.kind === 'database'
        ? 'Database error'
        : 'Something went wrong';
  const detail = 'message' in error ? error.message : `at ${error.path}`;
  return (
    <div className="error-box">
      <strong>{title}</strong>
      <p>{detail}</p>
      <button className="btn" type="button" onClick={onRetry}>
        Try again
      </button>
    </div>
  );
}
