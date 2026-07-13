import type { MouseEvent } from 'react';
import type { PendingReply } from '../lib/types';
import type { AgeTier } from '../lib/format';
import { initials } from '../lib/format';

interface RowProps {
  item: PendingReply;
  /** Non-null when the row is acknowledged: the short explanation
   *  shown in the meta line ("They reacted" / "You reacted, never
   *  replied"). Acknowledged rows render dimmed. */
  ackLabel: string | null;
  relative: string;
  tier: AgeTier;
  removing: boolean;
  selected: boolean;
  /** True when any row is selected - row clicks then toggle selection
   *  instead of opening the chat. */
  selectionActive: boolean;
  onComplete: () => void;
  onSnooze: () => void;
  onMute: () => void;
  onOpen: () => void;
  onToggleSelect: () => void;
}

export function Row({
  item,
  ackLabel,
  relative,
  tier,
  removing,
  selected,
  selectionActive,
  onComplete,
  onSnooze,
  onMute,
  onOpen,
  onToggleSelect,
}: RowProps) {
  const stop = (e: MouseEvent<HTMLButtonElement>, fn: () => void) => {
    e.stopPropagation();
    fn();
  };
  const activate = () => {
    if (selectionActive) {
      onToggleSelect();
    } else {
      onOpen();
    }
  };
  const rowTitle =
    item.isGroup && item.participants.length > 0
      ? item.participants.join(', ')
      : item.name;
  const senderPrefix = item.isGroup && item.lastSender ? `${item.lastSender}: ` : '';
  const previewText = item.preview ? `${senderPrefix}${item.preview}` : null;
  const fallbackMeta = item.isGroup && item.lastSender ? item.lastSender : 'Needs reply';
  return (
    <li
      className={`row${removing ? ' row--removing' : ''}${selected ? ' row--selected' : ''}${ackLabel ? ' row--acked' : ''}`}
      role="button"
      tabIndex={0}
      aria-selected={selected}
      title={rowTitle}
      onClick={activate}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          activate();
        }
      }}
    >
      <button
        className={`row__avatar${selected ? ' row__avatar--selected' : ''}`}
        type="button"
        aria-label={selected ? `Deselect ${item.name}` : `Select ${item.name}`}
        aria-pressed={selected}
        onClick={(e) => stop(e, onToggleSelect)}
        disabled={removing}
      >
        <span className="row__avatar-initials">{initials(item.name)}</span>
        <span className="row__avatar-check" aria-hidden>
          <CheckIcon />
        </span>
      </button>
      <div className="row__body">
        <div className="row__name">{item.name}</div>
        <div className="row__meta">
          {ackLabel && (
            <>
              <span className="row__ack">{ackLabel}</span>
              <span className="row__sep">·</span>
            </>
          )}
          <span className="row__preview">{previewText ?? fallbackMeta}</span>
          <span className="row__sep">·</span>
          <span className={`row__time row__time--${tier}`}>{relative}</span>
        </div>
      </div>
      <div className="row__actions">
        <button
          className="row__action row__action--secondary"
          type="button"
          aria-label={`Snooze ${item.name} until tomorrow morning`}
          title="Snooze until 9am"
          onClick={(e) => stop(e, onSnooze)}
          disabled={removing}
        >
          <MoonIcon />
        </button>
        <button
          className="row__action row__action--secondary"
          type="button"
          aria-label={`Mute ${item.name} permanently`}
          title="Mute this chat"
          onClick={(e) => stop(e, onMute)}
          disabled={removing}
        >
          <MuteIcon />
        </button>
        <button
          className="row__action"
          type="button"
          aria-label={`Mark ${item.name} as replied`}
          title="Mark as handled"
          onClick={(e) => stop(e, onComplete)}
          disabled={removing}
        >
          <CheckIcon />
        </button>
      </div>
    </li>
  );
}

function CheckIcon() {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden
    >
      <path
        d="M3.5 8.5L6.5 11.5L12.5 4.5"
        stroke="currentColor"
        strokeWidth="1.75"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function MoonIcon() {
  return (
    <svg
      width="15"
      height="15"
      viewBox="0 0 16 16"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden
    >
      <path
        d="M13.5 9.5A5.75 5.75 0 0 1 6.5 2.5a5.75 5.75 0 1 0 7 7Z"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function MuteIcon() {
  return (
    <svg
      width="15"
      height="15"
      viewBox="0 0 16 16"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden
    >
      <path
        d="M12.5 6.5a4.5 4.5 0 0 0-7.6-3.25M4 6.5c0 3.5-1.5 4.5-1.5 4.5h9M6.5 13.5a1.6 1.6 0 0 0 3 0"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M2 2l12 12"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
    </svg>
  );
}
