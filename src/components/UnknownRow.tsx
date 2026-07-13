import type { MouseEvent } from 'react';
import type { PressMods } from '../App';
import type { UnknownSender } from '../lib/types';

interface UnknownRowProps {
  item: UnknownSender;
  /** Relative time of the most recent / first message. */
  relative: string;
  firstRelative: string;
  removing: boolean;
  selected: boolean;
  onPress: (mods: PressMods) => void;
  onToggleSelect: (mods: PressMods) => void;
  onComplete: () => void;
  onAddToContacts: () => void;
}

/** A row in the unknown-senders list: the raw handle plus enough
 *  context (dates, count, latest snippet) to decide what to do. */
export function UnknownRow({
  item,
  relative,
  firstRelative,
  removing,
  selected,
  onPress,
  onToggleSelect,
  onComplete,
  onAddToContacts,
}: UnknownRowProps) {
  const stop = (e: MouseEvent<HTMLButtonElement>, fn: () => void) => {
    e.stopPropagation();
    fn();
  };
  const countLabel = item.messageCount === 1 ? '1 message' : `${item.messageCount} messages`;
  return (
    <li
      className={`row${removing ? ' row--removing' : ''}${selected ? ' row--selected' : ''}`}
      role="button"
      tabIndex={0}
      aria-selected={selected}
      title={item.handle}
      onClick={(e) => onPress({ metaKey: e.metaKey, shiftKey: e.shiftKey })}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onPress({ metaKey: e.metaKey, shiftKey: e.shiftKey });
        }
      }}
    >
      <button
        className={`row__avatar${selected ? ' row__avatar--selected' : ''}`}
        type="button"
        aria-label={selected ? `Deselect ${item.handle}` : `Select ${item.handle}`}
        aria-pressed={selected}
        onClick={(e) =>
          stop(e, () =>
            onToggleSelect({ metaKey: e.metaKey, shiftKey: e.shiftKey }),
          )
        }
        disabled={removing}
      >
        <span className="row__avatar-initials">?</span>
        <span className="row__avatar-check" aria-hidden>
          <CheckIcon />
        </span>
      </button>
      <div className="row__body">
        <div className="row__name">{item.handle}</div>
        <div className="row__meta">
          {item.preview ? (
            <>
              <span className="row__preview">{item.preview}</span>
              <span className="row__sep">·</span>
            </>
          ) : null}
          <span className={`row__time row__time--fresh`}>{relative}</span>
        </div>
        <div className="row__meta row__meta--secondary">
          <span>{countLabel}</span>
          <span className="row__sep">·</span>
          <span>first {firstRelative}</span>
        </div>
      </div>
      <div className="row__actions">
        <button
          className="row__action row__action--secondary"
          type="button"
          aria-label={`Add ${item.handle} to Contacts`}
          title="Add to Contacts"
          onClick={(e) => stop(e, onAddToContacts)}
          disabled={removing}
        >
          <PersonPlusIcon />
        </button>
        <button
          className="row__action"
          type="button"
          aria-label={`Mark ${item.handle} as handled`}
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

function PersonPlusIcon() {
  return (
    <svg
      width="15"
      height="15"
      viewBox="0 0 16 16"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden
    >
      <circle cx="6.5" cy="5" r="2.75" stroke="currentColor" strokeWidth="1.5" />
      <path
        d="M1.75 13.5c.7-2.2 2.6-3.5 4.75-3.5 1.1 0 2.13.34 2.97.95"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
      <path
        d="M12.5 9.5v4M10.5 11.5h4"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
    </svg>
  );
}
