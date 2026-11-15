import { useEffect, useRef, type KeyboardEvent } from "react";
import type { FolderRename } from "./useFolderRename";

/**
 * A folder's name, or the field to rename it. Enter or leaving the field saves; Escape cancels.
 */
export function FolderName({
  rename,
  ready,
}: {
  rename: FolderRename;
  ready: boolean;
}) {
  const { name, draft, start } = rename;
  if (draft === null) {
    return (
      <div className="row-title">
        <span
          className="name"
          title={ready ? "Double-click to rename" : undefined}
          onDoubleClick={start}
        >
          {name}
        </span>
      </div>
    );
  }
  return (
    <div className="row-title">
      <RenameField rename={rename} draft={draft} />
    </div>
  );
}

/** The rename field, focused on appearing with the old name selected to type over. */
function RenameField({
  rename,
  draft,
}: {
  rename: FolderRename;
  draft: string;
}) {
  const { setDraft, finish } = rename;
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    input.current?.focus();
    input.current?.select();
  }, []);

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === "Enter") finish(true);
    if (event.key === "Escape") finish(false);
  }

  return (
    <input
      ref={input}
      className="rename"
      type="text"
      spellCheck={false}
      aria-label="Folder name"
      value={draft}
      onChange={(event) => setDraft(event.currentTarget.value)}
      onKeyDown={onKeyDown}
      onBlur={() => finish(true)}
    />
  );
}
