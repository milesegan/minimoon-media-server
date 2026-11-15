import { Menu } from "@tauri-apps/api/menu";
import { LogicalPosition } from "@tauri-apps/api/dpi";
import { useState, type MouseEvent } from "react";
import * as commands from "../../backend/commands";
import type { FolderView } from "../../backend/types";
import { isOffline } from "../../lib/folder-status";
import { selectBusy, selectReady, useStore } from "../../store";
import { FolderIcon, MoreIcon } from "../icons";
import { FolderName } from "./FolderName";
import { useFolderRename } from "./useFolderRename";
import { FolderStatus } from "./FolderStatus";
import { ScanErrors } from "./ScanErrors";

/** What the file manager is called here, for the menu item that opens a folder in it. */
const OPEN_FOLDER_LABEL = /Mac/.test(navigator.userAgent)
  ? "Open in Finder"
  : /Windows/.test(navigator.userAgent)
    ? "Open in File Explorer"
    : "Open Folder";

/**
 * One shared folder: an icon, its name over its path, its status on the right, and an actions
 * menu that a right-click on the row also opens. Activity adds a progress bar underneath, and
 * scan errors a disclosure.
 */
export function FolderRow({ folder }: { folder: FolderView }) {
  const ready = useStore(selectReady);
  const busy = useStore(selectBusy);
  const running = useStore((state) => state.status?.phase === "running");
  const run = useStore((state) => state.run);
  const rename = useFolderRename(folder);
  const [menuOpen, setMenuOpen] = useState(false);
  const offline = isOffline(folder);

  async function showMenu(at: LogicalPosition) {
    if (busy || !running) return;
    const menu = await Menu.new({
      items: [
        { text: "Rename", action: rename.start },
        { text: "Rescan", action: () => run(commands.rescan, folder.id) },
        {
          text: OPEN_FOLDER_LABEL,
          action: () => run(commands.openFolder, folder.id),
        },
        { item: "Separator" },
        {
          text: "Stop Sharing…",
          action: () => run(commands.removeFolder, folder.id),
        },
      ],
    });
    setMenuOpen(true);
    try {
      await menu.popup(at);
    } finally {
      setMenuOpen(false);
    }
  }

  function onMoreClick(event: MouseEvent<HTMLButtonElement>) {
    const rect = event.currentTarget.getBoundingClientRect();
    showMenu(new LogicalPosition(rect.left, rect.bottom + 4));
  }

  function onContextMenu(event: MouseEvent) {
    // Leave text fields, the path and the error list their own menu, for copying.
    if ((event.target as Element).closest("input, .row-sub, details ul"))
      return;
    event.preventDefault();
    showMenu(new LogicalPosition(event.clientX, event.clientY));
  }

  return (
    <li
      className={offline ? "folder offline" : "folder"}
      onContextMenu={onContextMenu}
    >
      <div className="row">
        <span className="chip">
          <FolderIcon />
        </span>
        <div className="row-main">
          <FolderName rename={rename} ready={ready} />
          {/* The marks keep the path left to right inside the right-to-left box that trims its start. */}
          <div className="row-sub path" title={folder.path}>
            {`‎${folder.path}‎`}
          </div>
        </div>
        <FolderStatus folder={folder} />
        <button
          className="icon more"
          title="Folder actions"
          aria-label="Folder actions"
          aria-haspopup="menu"
          aria-expanded={menuOpen || undefined}
          disabled={!ready}
          onClick={onMoreClick}
        >
          <MoreIcon />
        </button>
      </div>
      {folder.activity && !offline && (
        <Progress label={folder.activity} progress={folder.progress} />
      )}
      <ScanErrors errors={folder.scanErrors} />
    </li>
  );
}

/** A thin bar under the row; it slides back and forth when there's nothing to count. */
function Progress({
  label,
  progress,
}: {
  label: string;
  progress: FolderView["progress"];
}) {
  const [done, total] = progress ?? [0, 0];
  const width = progress && total > 0 ? `${(100 * done) / total}%` : undefined;
  return (
    <div
      className={progress ? "progress" : "progress indeterminate"}
      role="progressbar"
      aria-label={label}
      aria-valuenow={progress ? done : undefined}
      aria-valuemax={progress ? total : undefined}
    >
      <div
        className="bar"
        style={progress ? { width: width ?? "0" } : undefined}
      />
    </div>
  );
}
