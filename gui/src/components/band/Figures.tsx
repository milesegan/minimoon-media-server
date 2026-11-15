import type { ServerView } from "../../backend/types";
import { plural } from "../../lib/format";
import { useStore } from "../../store";
import { CopyIcon } from "../icons";

/** The band's three figures: the address (click to copy), what's playing, and what's shared. */
export function Figures({ server }: { server: ServerView }) {
  const downloads = server.paused ? 0 : server.downloads;
  const shared = server.folders.filter((folder) => folder.shared);
  const files = shared.reduce((sum, folder) => sum + folder.files, 0);

  return (
    <dl className="figures">
      <div className="figure">
        <dt>Address</dt>
        <dd className="address-figure">
          <Address server={server} />
        </dd>
      </div>
      <div className="figure">
        <dt>Playing now</dt>
        <dd className={downloads === 0 ? "none" : undefined}>
          {downloads === 0 ? "Nothing" : plural(downloads, "stream")}
        </dd>
      </div>
      <div className="figure">
        <dt>Library</dt>
        <dd className={shared.length === 0 ? "none" : undefined}>
          {shared.length === 0 ? "Nothing shared" : plural(files, "file")}
        </dd>
      </div>
    </dl>
  );
}

/** The server's address without its scheme, as a button that copies the whole URL. */
function Address({ server }: { server: ServerView }) {
  const showToast = useStore((state) => state.showToast);
  const address = server.address;

  // Pausing closes the listener, so there's no address until sharing resumes.
  if (!address) {
    return (
      <span className="figure-none">
        {server.paused ? "Off while paused" : "Not available"}
      </span>
    );
  }

  async function copy(address: string) {
    try {
      await navigator.clipboard.writeText(address);
      showToast(`Copied ${address}`);
    } catch (error) {
      showToast(`Couldn't copy the address: ${error}`, { error: true });
    }
  }

  return (
    <button
      className="address"
      title="Copy the server's address"
      onClick={() => copy(address)}
    >
      <span className="address-text">
        {address.replace(/^https?:\/\//, "")}
      </span>
      <CopyIcon />
    </button>
  );
}
