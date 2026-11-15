import * as commands from "../../backend/commands";
import type { FolderView } from "../../backend/types";
import { selectReady, useStore } from "../../store";
import { Section } from "../common/Section";
import { FolderIcon, PlusIcon } from "../icons";
import { FolderRow } from "./FolderRow";

/** The shared folders, one row each, or an empty state that invites adding the first. */
export function FoldersSection({ folders }: { folders: FolderView[] }) {
  const ready = useStore(selectReady);
  const run = useStore((state) => state.run);
  const addFolder = () => run(commands.addFolder);

  const addButton = folders.length > 0 && (
    <button
      className="text-button accent"
      disabled={!ready}
      onClick={addFolder}
    >
      <PlusIcon className="plus" />
      <span>Add Folder…</span>
    </button>
  );

  return (
    <Section title="Shared folders" action={addButton}>
      {folders.length > 0 ? (
        <ul className="list">
          {folders.map((folder) => (
            <FolderRow key={folder.id} folder={folder} />
          ))}
        </ul>
      ) : (
        <div className="empty-state">
          <span className="chip large" aria-hidden="true">
            <FolderIcon />
          </span>
          <p className="empty-title">No shared folders yet</p>
          <p className="hint">
            Add a folder of music or video to share it with devices on this
            network.
          </p>
          <button className="primary" disabled={!ready} onClick={addFolder}>
            Add Folder…
          </button>
        </div>
      )}
    </Section>
  );
}
