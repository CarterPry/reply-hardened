interface PermissionPromptProps {
  path: string;
  onGrant: () => void;
  onRetry: () => void;
}

export function PermissionPrompt({ path, onGrant, onRetry }: PermissionPromptProps) {
  return (
    <div className="permission">
      <h2>Full Disk Access required</h2>
      <p>
        This app reads your iMessage history (read-only) to find threads waiting on a reply.
        macOS protects <code>{path}</code> behind Full Disk Access.
      </p>
      <ol>
        <li>
          Open <strong>System Settings → Privacy &amp; Security → Full Disk Access</strong>.
        </li>
        <li>Click the <strong>+</strong> button.</li>
        <li>
          Add <strong>Repli Hardened</strong>. In dev, also add your terminal or IDE
          (whichever launched <code>npm run tauri dev</code>).
        </li>
        <li>Quit and reopen the app, then click Try again.</li>
      </ol>
      <div className="permission__actions">
        <button className="btn btn--primary" type="button" onClick={onGrant}>
          Open Settings
        </button>
        <button className="btn" type="button" onClick={onRetry}>
          Try again
        </button>
      </div>
    </div>
  );
}
