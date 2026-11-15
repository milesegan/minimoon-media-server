import { useRef, useState } from "react";
import * as commands from "../../backend/commands";
import type { FolderView } from "../../backend/types";
import { selectBusy, useStore } from "../../store";

export interface FolderRename {
  /** The name to show: the new one while it's saving, else the folder's. */
  name: string;
  /** What's typed in the rename field, or null when not renaming. */
  draft: string | null;
  setDraft: (draft: string) => void;
  /** Starts editing, unless a command is in flight. */
  start: () => void;
  /** Stops editing, saving the draft if `save` and it's a new, non-empty name. */
  finish: (save: boolean) => void;
}

/** Renaming a folder in place: started by a double-click or the actions menu. */
export function useFolderRename(folder: FolderView): FolderRename {
  const run = useStore((state) => state.run);
  const busy = useStore(selectBusy);
  const [draft, setDraft] = useState<string | null>(null);
  // The new name, shown from saving until the refreshed status has it.
  const [savingName, setSavingName] = useState<string | null>(null);
  // Escape and the blur that follows it would both finish the edit; only the first counts.
  const editing = useRef(false);
  const name = savingName ?? folder.name;

  function start() {
    if (busy || editing.current) return;
    editing.current = true;
    setDraft(name);
  }

  async function finish(save: boolean) {
    if (!editing.current) return;
    editing.current = false;
    const newName = draft?.trim() ?? "";
    setDraft(null);
    if (!save || !newName || newName === name) return;
    setSavingName(newName);
    try {
      await run(commands.renameFolder, folder.id, newName);
    } finally {
      setSavingName(null);
    }
  }

  return { name, draft, setDraft, start, finish };
}
