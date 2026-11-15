import * as commands from "../../backend/commands";
import type { DeviceView } from "../../backend/types";
import { formatDate, pairedText } from "../../lib/format";
import { selectReady, useStore } from "../../store";
import { ComputerIcon, PhoneIcon, TabletIcon, TvIcon } from "../icons";

/** A paired device: what kind it is, its name, its app and when it paired, and Revoke. */
export function DeviceRow({ device }: { device: DeviceView }) {
  const ready = useStore(selectReady);
  const run = useStore((state) => state.run);

  return (
    <li>
      <div className="row">
        <span className="chip neutral">
          <DeviceIcon platform={device.platform} />
        </span>
        <div className="row-main">
          <div className="row-title">{device.clientName}</div>
          <div
            className="row-sub"
            title={`Paired ${formatDate(device.pairedAt)}`}
          >
            {device.app} on {device.platform} · {pairedText(device.pairedAt)}
          </div>
        </div>
        <button
          className="text-button"
          aria-label={`Revoke ${device.clientName}…`}
          disabled={!ready}
          onClick={() => run(commands.revokeDevice, device.clientId)}
        >
          Revoke…
        </button>
      </div>
    </li>
  );
}

/** A picture for the device, guessed from the free-form platform its app reports. */
function DeviceIcon({ platform }: { platform: string }) {
  if (/tv|roku|tizen|webos|fire ?stick/i.test(platform)) return <TvIcon />;
  if (/ipad|tablet/i.test(platform)) return <TabletIcon />;
  if (/mac|windows|linux|desktop|chrome ?os|web/i.test(platform))
    return <ComputerIcon />;
  return <PhoneIcon />;
}
