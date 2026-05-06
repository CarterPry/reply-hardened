import type { MouseEvent } from 'react';
import type { PendingReply } from '../lib/types';
import { initials } from '../lib/format';

interface RowProps {
  item: PendingReply;
  relative: string;
  removing: boolean;
  onComplete: () => void;
  onOpen: () => void;
}

export function Row({ item, relative, removing, onComplete, onOpen }: RowProps) {
  const handleAction = (e: MouseEvent<HTMLButtonElement>) => {
    e.stopPropagation();
    onComplete();
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
      className={`row${removing ? ' row--removing' : ''}`}
      role="button"
      tabIndex={0}
      title={rowTitle}
      onClick={onOpen}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onOpen();
        }
      }}
    >
      <div className="row__avatar" aria-hidden>
        <span>{initials(item.name)}</span>
      </div>
      <div className="row__body">
        <div className="row__name">{item.name}</div>
        <div className="row__meta">
          <span className="row__preview">{previewText ?? fallbackMeta}</span>
          <span className="row__sep">·</span>
          <span className="row__time">{relative}</span>
        </div>
      </div>
      <button
        className="row__action"
        type="button"
        aria-label={`Mark ${item.name} as replied`}
        onClick={handleAction}
        disabled={removing}
      >
        <CheckIcon />
      </button>
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
