import type { FolderView } from "../../backend/types";
import { describeFolder, type StatusLine } from "../../lib/folder-status";

/**
 * The right-hand column of a folder row: the file count when all is well, otherwise what's
 * going on. The full text is in the tooltip, since long reasons are cut short.
 */
export function FolderStatus({ folder }: { folder: FolderView }) {
  const status = describeFolder(folder);

  if (status.kind === "offline") {
    return (
      <div className="status" title={`Offline\n${status.reason}`}>
        <div className="main">
          <span className="pill">Offline</span>
        </div>
        <div className="sub">{status.reason}</div>
      </div>
    );
  }

  const { main, sub } = status;
  return (
    <div
      className="status"
      title={[main.text, sub?.text].filter(Boolean).join("\n")}
    >
      <Line line={main} className="main" />
      {sub && <Line line={sub} className="sub" />}
    </div>
  );
}

function Line({ line, className }: { line: StatusLine; className: string }) {
  const tone = line.tone === "plain" ? "" : ` ${line.tone}`;
  return <div className={className + tone}>{line.text}</div>;
}
