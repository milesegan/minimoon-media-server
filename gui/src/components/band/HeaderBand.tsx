import * as commands from "../../backend/commands";
import type { Status } from "../../backend/types";
import { selectReady, useStore } from "../../store";
import { MoonIcon } from "../icons";
import { Figures } from "./Figures";

/**
 * The navy band at the top: the cream moon, the server's name and whether it's sharing,
 * Pause/Resume Sharing, and the figures underneath while the server runs.
 */
export function HeaderBand() {
  const status = useStore((state) => state.status);
  const ready = useStore(selectReady);
  const run = useStore((state) => state.run);
  const server = status?.server ?? null;
  const [state, stateText] = describeState(status);

  return (
    <header className="band">
      <div className="band-top">
        <MoonIcon className="moon" />
        <div className="identity">
          {/* Until the server runs there's no server name, so the band shows the app's. */}
          <h1 title={server?.name}>
            {server?.name ?? "MiniMoon Media Server"}
          </h1>
          <div className={`state ${state}`}>
            <span className="dot" aria-hidden="true" />
            <span>{stateText}</span>
          </div>
        </div>
        {server && (
          <button
            // Paused, resuming is the one thing to do, so its button stands out.
            className={server.paused ? "band-button resume" : "band-button"}
            disabled={!ready}
            onClick={() => run(commands.setPaused, !server.paused)}
          >
            {server.paused ? "Resume Sharing" : "Pause Sharing"}
          </button>
        )}
      </div>
      {server && <Figures server={server} />}
    </header>
  );
}

/** A class for the state dot, and the line beside it. */
function describeState(status: Status | null): [string, string] {
  if (!status || status.phase === "starting") return ["starting", "Starting…"];
  if (status.phase === "failed") return ["failed", "Not running"];
  if (status.server?.paused) return ["paused", "Paused"];
  return ["sharing", "Sharing on this network"];
}
