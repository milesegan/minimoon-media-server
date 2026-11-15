import { useStore } from "../store";

/** Why the server couldn't start, or that sharing is paused, under the band. */
export function Banners() {
  const status = useStore((state) => state.status);
  return (
    <>
      {status?.phase === "failed" && (
        <div className="banner failure">
          <strong>The server couldn't start.</strong>
          <span className="selectable">{status.error}</span>
        </div>
      )}
      {status?.server?.paused && (
        <div className="banner paused">
          <strong>Sharing is paused.</strong>
          <span>
            Devices can't browse or play anything here until you resume.
          </span>
        </div>
      )}
    </>
  );
}
