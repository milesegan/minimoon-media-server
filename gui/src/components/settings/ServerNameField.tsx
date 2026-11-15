import { useId, useRef, useState, type KeyboardEvent } from "react";
import * as commands from "../../backend/commands";
import type { ServerView } from "../../backend/types";
import { selectReady, useStore } from "../../store";

/**
 * The name devices see. Empty means the computer's own name, shown as the placeholder. It saves
 * on Enter or on leaving the field, and Escape puts the saved name back.
 */
export function ServerNameField({ server }: { server: ServerView }) {
  const ready = useStore(selectReady);
  const run = useStore((state) => state.run);
  const inputId = useId();
  const hintId = useId();
  // While the field has focus it holds what's being typed; otherwise it follows the server.
  const [draft, setDraft] = useState<string | null>(null);
  // A name being saved, shown until the refreshed status has it.
  const [saving, setSaving] = useState<string | null>(null);
  const cancelled = useRef(false);

  const saved = server.customName ?? "";
  const editing = draft !== null;

  async function onBlur() {
    const value = draft ?? saved;
    const save = !cancelled.current && value !== saved;
    cancelled.current = false;
    setDraft(null);
    if (!save) return;
    setSaving(value);
    try {
      await run(commands.setName, value);
    } finally {
      setSaving(null);
    }
  }

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === "Enter") event.currentTarget.blur();
    if (event.key === "Escape") {
      cancelled.current = true;
      event.currentTarget.blur();
    }
  }

  return (
    <div className="setting wrap">
      <label htmlFor={inputId} className="setting-title">
        Name shown to devices
      </label>
      <input
        id={inputId}
        type="text"
        spellCheck={false}
        autoComplete="off"
        aria-describedby={hintId}
        placeholder={server.hostName}
        // Stays usable while focused, so a command finishing doesn't yank it away mid-edit.
        disabled={!ready && !editing}
        value={draft ?? saving ?? saved}
        onFocus={() => setDraft(saved)}
        onChange={(event) => setDraft(event.currentTarget.value)}
        onKeyDown={onKeyDown}
        onBlur={onBlur}
      />
      <span id={hintId} className="visually-hidden">
        Leave it empty to use this computer's name.
      </span>
    </div>
  );
}
