import { useCallback, useEffect, useRef, useState } from 'react';
import { Row } from './components/Row';
import { PermissionPrompt } from './components/PermissionPrompt';
import {
  getPendingReplies,
  isAppError,
  markComplete,
  openChat,
  openFdaSettings,
  refresh as refreshApi,
} from './lib/api';
import { formatRelative } from './lib/format';
import { notifyNew } from './lib/notifications';
import type { AppError, PendingReply } from './lib/types';

const POLL_MS = 30_000;

export function App() {
  const [items, setItems] = useState<PendingReply[]>([]);
  const [error, setError] = useState<AppError | null>(null);
  const [loading, setLoading] = useState(true);
  const [removing, setRemoving] = useState<Set<string>>(new Set());
  /** Tracks the set of lastMessageGuids we've already shown to the user.
   *  Stays null until the first successful load — so the very first
   *  result doesn't trigger a flood of notifications. */
  const seenGuidsRef = useRef<Set<string> | null>(null);

  const applyResults = useCallback((data: PendingReply[]) => {
    setItems(data);
    setError(null);
    const nextSeen = new Set(data.map((x) => x.lastMessageGuid));
    const prevSeen = seenGuidsRef.current;
    if (prevSeen) {
      const fresh = data.filter((x) => !prevSeen.has(x.lastMessageGuid));
      if (fresh.length > 0) {
        notifyNew(fresh).catch((e) => console.error('notify failed', e));
      }
    }
    seenGuidsRef.current = nextSeen;
  }, []);

  const handleError = useCallback((e: unknown) => {
    setError(isAppError(e) ? e : { kind: 'other', message: String(e) });
  }, []);

  const refreshAll = useCallback(async () => {
    try {
      const data = await refreshApi();
      applyResults(data);
    } catch (e) {
      handleError(e);
    }
  }, [applyResults, handleError]);

  const retry = useCallback(async () => {
    setLoading(true);
    try {
      const data = await getPendingReplies();
      applyResults(data);
    } catch (e) {
      handleError(e);
    } finally {
      setLoading(false);
    }
  }, [applyResults, handleError]);

  useEffect(() => {
    let active = true;
    getPendingReplies()
      .then((data) => {
        if (active) applyResults(data);
      })
      .catch((e) => {
        if (active) handleError(e);
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

  // Poll every 30s while the window is visible. Hidden windows skip
  // the tick — they'll resync via visibilitychange when the user
  // returns. New pendings discovered during a poll fire notifications
  // via the diff in applyResults.
  useEffect(() => {
    const id = window.setInterval(() => {
      if (document.visibilityState === 'hidden') return;
      refreshAll();
    }, POLL_MS);
    return () => window.clearInterval(id);
  }, [refreshAll]);

  const onComplete = useCallback(async (item: PendingReply) => {
    setRemoving((prev) => {
      const next = new Set(prev);
      next.add(item.lastMessageGuid);
      return next;
    });
    try {
      await markComplete(item.lastMessageGuid);
    } catch (e) {
      console.error('mark_complete failed', e);
      setRemoving((prev) => {
        const next = new Set(prev);
        next.delete(item.lastMessageGuid);
        return next;
      });
      return;
    }
    // Tell the seen-set we no longer have this one so a future revival
    // (a new guid) is correctly detected as "new" by the notify diff.
    if (seenGuidsRef.current) {
      seenGuidsRef.current.delete(item.lastMessageGuid);
    }
    setTimeout(() => {
      setItems((prev) => prev.filter((x) => x.lastMessageGuid !== item.lastMessageGuid));
      setRemoving((prev) => {
        const next = new Set(prev);
        next.delete(item.lastMessageGuid);
        return next;
      });
    }, 340);
  }, []);

  return (
    <div className="container">
      <header className="header">
        <h1>Replies</h1>
        {!loading && !error && items.length > 0 && (
          <span className="count">{items.length}</span>
        )}
      </header>
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
          <div className="empty">
            <div className="empty__title">All clear</div>
            <p>No conversations are waiting on you.</p>
          </div>
        ) : (
          <ul className="list">
            {items.map((item) => (
              <Row
                key={item.lastMessageGuid}
                item={item}
                relative={formatRelative(item.timestamp)}
                removing={removing.has(item.lastMessageGuid)}
                onComplete={() => onComplete(item)}
                onOpen={() => {
                  openChat(item.handle, item.isGroup, item.chatGuid).catch((e) =>
                    console.error('open_chat failed', e),
                  );
                }}
              />
            ))}
          </ul>
        )}
      </main>
    </div>
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
