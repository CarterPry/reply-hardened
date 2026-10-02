import { useCallback, useEffect, useRef, useState } from 'react';
import { Row } from './components/Row';
import { UnknownRow } from './components/UnknownRow';
import { PermissionPrompt } from './components/PermissionPrompt';
import {
  addToContacts,
  getReplies,
  getSources,
  getUnknownSenders,
  isAppError,
  listMuted,
  markDone,
  muteChats,
  openAccessibilitySettings,
  openAutomationSettings,
  openChat,
  openFdaSettings,
  snoozeChats,
  undoLast,
  unmuteChat,
} from './lib/api';
import { ageTier, formatRelative, nextMorning } from './lib/format';
import { notifyNew } from './lib/notifications';
import type {
  AppError,
  MutedChat,
  PendingReply,
  SourceStatus,
  UnknownSender,
  ViewMode,
} from './lib/types';
import { PLATFORM_LABELS } from './lib/types';

const POLL_MS = 30_000;
/** How long the post-action toast stays up. ⌘Z reaches much further
 *  back (the persisted action log), the toast is just the affordance. */
const TOAST_MS = 5_000;

interface Lists {
  owed: PendingReply[];
  waiting: PendingReply[];
}

/** Anything a list can render and a triage action can target. */
type ListItem = PendingReply | UnknownSender;

type TriageKind = 'done' | 'snooze' | 'mute';

/** Which permission the group-chat opener was denied, if any. */
type PermissionBanner = 'accessibility' | 'automation' | null;

interface Toast {
  message: string;
  canUndo: boolean;
}

function isReply(item: ListItem): item is PendingReply {
  return 'lastMessageGuid' in item;
}

/** The date of the item's last real message - what mark-done pins. */
function messageDateOf(item: ListItem): number {
  return isReply(item) ? item.timestamp : item.lastDate;
}

function displayNameOf(item: ListItem): string {
  return isReply(item) ? item.name : item.handle;
}

/** Click modifiers a row forwards so the list can pick between open,
 *  toggle-select (⌘) and range-select (⇧). */
export interface PressMods {
  metaKey: boolean;
  shiftKey: boolean;
}

export function App() {
  const [mode, setMode] = useState<ViewMode>('owed');
  const [lists, setLists] = useState<Lists>({ owed: [], waiting: [] });
  const [unknown, setUnknown] = useState<UnknownSender[]>([]);
  const [muted, setMuted] = useState<MutedChat[]>([]);
  const [sources, setSources] = useState<SourceStatus[]>([]);
  const [showMuted, setShowMuted] = useState(false);
  const [banner, setBanner] = useState<PermissionBanner>(null);
  const [error, setError] = useState<AppError | null>(null);
  const [loading, setLoading] = useState(true);
  const [removing, setRemoving] = useState<Set<string>>(new Set());
  /** chatGuids of rows selected for a bulk action (rows are unique per
   *  chat in every list). */
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [toast, setToast] = useState<Toast | null>(null);
  const toastTimerRef = useRef<number | null>(null);
  /** Index of the last individually-toggled row - the anchor a
   *  shift-click extends from, standard macOS list behavior. */
  const anchorIndexRef = useRef<number | null>(null);
  /** Tracks the set of lastMessageGuids we've already shown to the user.
   *  Stays null until the first successful load - so the very first
   *  result doesn't trigger a flood of notifications. Only the owed
   *  list notifies; "waiting" threads are by definition not news. */
  const seenGuidsRef = useRef<Set<string> | null>(null);
  /** Monotonic fetch counter. Poll, focus, and visibility refreshes
   *  overlap; only the newest in-flight fetch may apply its results,
   *  otherwise a slow stale response lands last and the list flickers
   *  backwards. */
  const fetchSeqRef = useRef(0);

  const applyResults = useCallback(
    (
      owed: PendingReply[],
      waiting: PendingReply[],
      unknownList: UnknownSender[],
      mutedList: MutedChat[],
      sourceList: SourceStatus[],
    ) => {
      setLists({ owed, waiting });
      setUnknown(unknownList);
      setMuted(mutedList);
      setSources(sourceList);
      setError(null);
      // Drop selections for rows that vanished in this refresh.
      const alive = new Set([
        ...owed.map((x) => x.chatGuid),
        ...waiting.map((x) => x.chatGuid),
        ...unknownList.map((x) => x.chatGuid),
      ]);
      setSelected((prev) => {
        const next = new Set([...prev].filter((g) => alive.has(g)));
        return next.size === prev.size ? prev : next;
      });
      const nextSeen = new Set(owed.map((x) => x.lastMessageGuid));
      const prevSeen = seenGuidsRef.current;
      if (prevSeen) {
        // Acknowledged rows are not news - you already reacted.
        const fresh = owed.filter(
          (x) => !prevSeen.has(x.lastMessageGuid) && !x.acknowledged,
        );
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

  const fetchAll = useCallback(async () => {
    const [owed, waiting, unknownList, mutedList] = await Promise.all([
      getReplies('owed'),
      getReplies('waiting'),
      getUnknownSenders(),
      listMuted(),
    ]);
    // Strictly after the reply fetches: get_replies is what refreshes
    // the per-source status, so asking concurrently would read the
    // previous poll's answer (and nothing at all on first launch).
    const sourceList = await getSources();
    return [owed, waiting, unknownList, mutedList, sourceList] as const;
  }, []);

  const refreshAll = useCallback(async () => {
    const seq = ++fetchSeqRef.current;
    try {
      const [owed, waiting, unknownList, mutedList, sourceList] = await fetchAll();
      if (seq === fetchSeqRef.current) {
        applyResults(owed, waiting, unknownList, mutedList, sourceList);
      }
    } catch (e) {
      if (seq === fetchSeqRef.current) handleError(e);
    }
  }, [fetchAll, applyResults, handleError]);

  const retry = useCallback(async () => {
    setLoading(true);
    await refreshAll();
    setLoading(false);
  }, [refreshAll]);

  useEffect(() => {
    let active = true;
    const seq = ++fetchSeqRef.current;
    fetchAll()
      .then(([owed, waiting, unknownList, mutedList, sourceList]) => {
        if (active && seq === fetchSeqRef.current) {
          applyResults(owed, waiting, unknownList, mutedList, sourceList);
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
  }, [fetchAll, applyResults, handleError]);

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

  const showToast = useCallback((message: string, canUndo: boolean) => {
    if (toastTimerRef.current !== null) {
      window.clearTimeout(toastTimerRef.current);
    }
    setToast({ message, canUndo });
    toastTimerRef.current = window.setTimeout(() => {
      setToast(null);
      toastTimerRef.current = null;
    }, TOAST_MS);
  }, []);

  /** Run one triage action against `targets` as a SINGLE backend call
   *  (one undo entry), animating the rows out together. Failures put
   *  the rows back via a resync. */
  const triage = useCallback(
    async (kind: TriageKind, targets: ListItem[]) => {
      if (targets.length === 0) return;
      const guids = targets.map((x) => x.chatGuid);
      setRemoving((prev) => new Set([...prev, ...guids]));
      setSelected(new Set());
      anchorIndexRef.current = null;
      try {
        if (kind === 'done') {
          await markDone(
            targets.map((x) => ({
              chatGuid: x.chatGuid,
              messageDate: messageDateOf(x),
            })),
          );
        } else if (kind === 'snooze') {
          await snoozeChats(guids, nextMorning());
        } else {
          await muteChats(
            targets.map((x) => ({
              chatGuid: x.chatGuid,
              name: displayNameOf(x),
            })),
          );
          setMuted(await listMuted());
        }
      } catch (e) {
        console.error(`${kind} failed`, e);
        setRemoving((prev) => {
          const next = new Set(prev);
          guids.forEach((g) => next.delete(g));
          return next;
        });
        refreshAll();
        return;
      }
      // The seen-set keeps these guids: a genuine revival arrives with
      // a NEW message guid (so it still notifies), while an undo brings
      // back the same guid and must NOT re-notify.
      const n = targets.length;
      const label =
        kind === 'done'
          ? n > 1
            ? `Marked ${n} done`
            : 'Marked done'
          : kind === 'snooze'
            ? n > 1
              ? `Snoozed ${n} until 9am`
              : 'Snoozed until 9am'
            : n > 1
              ? `Muted ${n}`
              : 'Muted';
      showToast(label, true);
      setTimeout(() => {
        const gone = new Set(guids);
        setLists((prev) => ({
          owed: prev.owed.filter((x) => !gone.has(x.chatGuid)),
          waiting: prev.waiting.filter((x) => !gone.has(x.chatGuid)),
        }));
        setUnknown((prev) => prev.filter((x) => !gone.has(x.chatGuid)));
        setRemoving((prev) => {
          const next = new Set(prev);
          guids.forEach((g) => next.delete(g));
          return next;
        });
      }, 340);
    },
    [refreshAll, showToast],
  );

  const undo = useCallback(async () => {
    if (toastTimerRef.current !== null) {
      window.clearTimeout(toastTimerRef.current);
      toastTimerRef.current = null;
    }
    setToast(null);
    try {
      const outcome = await undoLast();
      if (!outcome) return;
      await refreshAll();
      showToast(
        outcome.count > 1 ? `Restored ${outcome.count} chats` : 'Restored',
        false,
      );
    } catch (e) {
      console.error('undo failed', e);
    }
  }, [refreshAll, showToast]);

  const visible: ListItem[] = mode === 'unknown' ? unknown : lists[mode];
  /** Label every row with its source only when the list actually spans
   *  more than one - on an iMessage-only Mac the chip is pure noise. */
  const showPlatform = new Set(visible.map((x) => x.platform)).size > 1;
  /** Sources that are silent for a reason the user can act on. A
   *  platform whose app simply isn't installed is not a problem to
   *  report, so `absent` is left out. */
  const degraded = sources.filter(
    (s) => s.state === 'blocked' || s.state === 'failed',
  );

  const clearSelection = useCallback(() => {
    setSelected(new Set());
    anchorIndexRef.current = null;
  }, []);

  const toggleSelect = useCallback((item: ListItem, index: number) => {
    anchorIndexRef.current = index;
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(item.chatGuid)) {
        next.delete(item.chatGuid);
      } else {
        next.add(item.chatGuid);
      }
      return next;
    });
  }, []);

  /** Shift-click: select the whole range between the anchor and the
   *  clicked row (inclusive), keeping whatever else was selected. */
  const rangeSelect = useCallback(
    (index: number) => {
      const anchor = anchorIndexRef.current ?? index;
      const [lo, hi] = anchor <= index ? [anchor, index] : [index, anchor];
      setSelected((prev) => {
        const next = new Set(prev);
        for (let i = lo; i <= hi; i++) {
          const item = visible[i];
          if (item) next.add(item.chatGuid);
        }
        return next;
      });
    },
    [visible],
  );

  const handlePress = useCallback(
    (item: ListItem, index: number, mods: PressMods) => {
      if (mods.shiftKey) {
        rangeSelect(index);
      } else if (mods.metaKey || selected.size > 0) {
        toggleSelect(item, index);
      } else if (isReply(item)) {
        openChat(
          item.platform,
          item.handle,
          item.isGroup,
          item.chatGuid,
          item.searchHint,
        ).catch(
          (e) => {
            const msg = String(e);
            if (msg.includes('accessibility_denied')) {
              setBanner('accessibility');
            } else if (msg.includes('automation_denied')) {
              setBanner('automation');
            } else {
              console.error('open_chat failed', e);
            }
          },
        );
      } else {
        openChat(
          item.platform,
          item.handle,
          false,
          item.chatGuid,
          item.handle,
        ).catch((e) => console.error('open_chat failed', e));
      }
    },
    [selected, rangeSelect, toggleSelect],
  );

  /** Switch tabs and drop the selection - it belongs to one list. */
  const switchMode = useCallback(
    (next: ViewMode) => {
      setMode(next);
      clearSelection();
    },
    [clearSelection],
  );

  // Esc clears selection; ⌘Z undoes; ⌘A selects all visible rows in
  // the current tab (filtered-out rows are not in `visible`, so they
  // are never swept in).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        clearSelection();
      } else if (e.metaKey && !e.shiftKey && e.key.toLowerCase() === 'z') {
        e.preventDefault();
        undo();
      } else if (e.metaKey && e.key.toLowerCase() === 'a') {
        e.preventDefault();
        setSelected(new Set(visible.map((x) => x.chatGuid)));
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [clearSelection, undo, visible]);

  const bulkTargets = useCallback(
    () => visible.filter((x) => selected.has(x.chatGuid)),
    [visible, selected],
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

  const onAddToContacts = useCallback((item: UnknownSender) => {
    addToContacts(item.handle).catch((e) =>
      console.error('add_to_contacts failed', e),
    );
  }, []);

  const selCount = selected.size;

  return (
    <div className="container">
      <header className="header">
        <h1>Replies</h1>
        <nav className="tabs" aria-label="Reply lists">
          <TabButton
            label="Owe reply"
            count={lists.owed.filter((x) => !x.acknowledged).length}
            active={mode === 'owed'}
            onClick={() => switchMode('owed')}
          />
          <TabButton
            label="Waiting"
            count={lists.waiting.filter((x) => !x.acknowledged).length}
            active={mode === 'waiting'}
            onClick={() => switchMode('waiting')}
          />
          <TabButton
            label="Unknown"
            count={unknown.length}
            active={mode === 'unknown'}
            onClick={() => switchMode('unknown')}
          />
        </nav>
      </header>
      {banner && (
        <div className="banner">
          <p>
            {banner === 'accessibility'
              ? 'Opening group chats needs the Accessibility permission. Enable "Repli Hardened" under System Settings → Privacy & Security → Accessibility, then try again.'
              : 'Opening group chats needs permission to control Messages. Allow "Repli Hardened" under System Settings → Privacy & Security → Automation, then try again.'}
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
        ) : visible.length === 0 ? (
          mode === 'owed' ? (
            <div className="empty">
              <div className="empty__title">All clear</div>
              <p>No conversations are waiting on you.</p>
            </div>
          ) : mode === 'waiting' ? (
            <div className="empty">
              <div className="empty__title">Nothing outstanding</div>
              <p>No unanswered messages older than 3 days.</p>
            </div>
          ) : (
            <div className="empty">
              <div className="empty__title">No unknown senders</div>
              <p>Everyone who messaged you recently is in your Contacts.</p>
            </div>
          )
        ) : (
          <ul className="list">
            {mode === 'unknown'
              ? unknown.map((item, index) => (
                  <UnknownRow
                    key={item.chatGuid}
                    item={item}
                    showPlatform={showPlatform}
                    relative={formatRelative(item.lastDate)}
                    firstRelative={formatRelative(item.firstDate)}
                    removing={removing.has(item.chatGuid)}
                    selected={selected.has(item.chatGuid)}
                    onPress={(mods) => handlePress(item, index, mods)}
                    onToggleSelect={(mods) =>
                      mods.shiftKey ? rangeSelect(index) : toggleSelect(item, index)
                    }
                    onComplete={() => triage('done', [item])}
                    onAddToContacts={() => onAddToContacts(item)}
                  />
                ))
              : lists[mode].map((item, index) => (
                  <Row
                    key={item.chatGuid}
                    item={item}
                    showPlatform={showPlatform}
                    ackLabel={
                      item.acknowledged
                        ? mode === 'owed'
                          ? 'You reacted, never replied'
                          : 'They reacted'
                        : null
                    }
                    relative={formatRelative(item.timestamp)}
                    tier={ageTier(item.timestamp)}
                    removing={removing.has(item.chatGuid)}
                    selected={selected.has(item.chatGuid)}
                    onPress={(mods) => handlePress(item, index, mods)}
                    onToggleSelect={(mods) =>
                      mods.shiftKey ? rangeSelect(index) : toggleSelect(item, index)
                    }
                    onComplete={() => triage('done', [item])}
                    onSnooze={() => triage('snooze', [item])}
                    onMute={() => triage('mute', [item])}
                  />
                ))}
          </ul>
        )}
        {selCount > 0 && (
          <div className="selection-bar">
            <span className="selection-bar__count">{selCount} selected</span>
            <div className="selection-bar__actions">
              <button
                className="btn btn--small"
                type="button"
                onClick={() => triage('snooze', bulkTargets())}
              >
                Snooze {selCount}
              </button>
              <button
                className="btn btn--small"
                type="button"
                onClick={() => triage('mute', bulkTargets())}
              >
                Mute {selCount}
              </button>
              <button
                className="btn btn--small btn--primary"
                type="button"
                onClick={() => triage('done', bulkTargets())}
              >
                Mark {selCount} done
              </button>
              <button
                className="btn btn--small"
                type="button"
                aria-label="Cancel selection"
                onClick={clearSelection}
              >
                ✕
              </button>
            </div>
          </div>
        )}
        {toast && (
          <div className="toast" role="status">
            <span className="toast__message">{toast.message}</span>
            {toast.canUndo && (
              <button className="btn btn--small" type="button" onClick={undo}>
                Undo
              </button>
            )}
          </div>
        )}
        {!loading && !error && degraded.length > 0 && (
          <div className="source-note" role="status">
            {degraded.map((s) => (
              <div className="source-note__line" key={s.platform}>
                <span className="source-note__platform">
                  {PLATFORM_LABELS[s.platform]}
                </span>
                <span className="source-note__detail">
                  {s.detail ?? 'not being read'}
                </span>
              </div>
            ))}
          </div>
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
