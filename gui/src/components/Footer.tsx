import * as commands from "../backend/commands";
import { FAILED, selectReady, useStore } from "../store";

/**
 * Maintenance that's rarely needed, as quiet links: rescanning and reindexing while the server
 * runs, and opening the data folder at any time.
 */
export function Footer() {
  const status = useStore((state) => state.status);
  const ready = useStore(selectReady);
  const run = useStore((state) => state.run);
  const showToast = useStore((state) => state.showToast);

  async function reindex() {
    if ((await run(commands.reindexEverything)) !== FAILED)
      showToast("Reindexing in the background");
  }

  // Not run through `run`: it shouldn't wait on the server or disable anything, since after a
  // failed start the data folder is where to look.
  async function showDataFolder() {
    try {
      await commands.showDataFolder();
    } catch (error) {
      showToast(String(error), { error: true });
    }
  }

  return (
    <footer>
      {status?.server && (
        <div className="maintenance">
          <button
            className="text-button"
            title="Check every folder for changes now. Folders are watched, so this is rarely needed."
            disabled={!ready}
            onClick={() => run(commands.rescan, null)}
          >
            Rescan all folders
          </button>
          <span className="separator" aria-hidden="true">
            ·
          </span>
          <button
            className="text-button"
            title="Re-read every file's tags and video details in the background."
            disabled={!ready}
            onClick={reindex}
          >
            Reindex everything
          </button>
        </div>
      )}
      {status?.dataDir && (
        <button
          className="text-button data-folder"
          title={`Settings and index: ${status.dataDir}`}
          onClick={showDataFolder}
        >
          Show data folder
        </button>
      )}
    </footer>
  );
}
