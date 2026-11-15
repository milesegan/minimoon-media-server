import { useEffect, useState } from "react";
import { formatTimeLeft } from "../../lib/format";
import { useStore, type Pairing } from "../../store";
import { CloseIcon } from "../icons";

/** Who's asking, the code to enter on their device, and how long the code is good for. */
export function PairingCard({ pairing }: { pairing: Pairing }) {
  const { request, shown } = pairing;
  const removePairing = useStore((state) => state.removePairing);
  const now = useClock();

  const lifetime = request.expiresInSecs * 1000;
  const expires = shown + lifetime;
  const secondsLeft = Math.max(0, Math.round((expires - now) / 1000));
  // Aimed a second ahead: the bar's one-second transition gets it there by the next tick.
  const fraction = Math.max(0, (expires - now - 1000) / lifetime);
  const title = `${request.clientName} wants to connect`;

  useEffect(() => {
    if (secondsLeft === 0) removePairing(request.pairingId);
  }, [secondsLeft, removePairing, request.pairingId]);

  return (
    <div className="pairing" role="group" aria-label={title}>
      <div className="who">
        <strong className="title">{title}</strong>
        <span className="instruction">
          Enter this code in {request.app} on the {request.platform} device.
        </span>
        {/* Quiet inside the live region: announce the card once, not every second. */}
        <div className="time" aria-live="off">
          <div className="track" aria-hidden="true">
            <div className="bar" style={{ transform: `scaleX(${fraction})` }} />
          </div>
          <span>{formatTimeLeft(secondsLeft)} left</span>
        </div>
      </div>
      <div className="code-and-close">
        <Code code={request.code} />
        <button
          className="icon dismiss"
          title="Dismiss"
          aria-label="Dismiss"
          onClick={() => removePairing(request.pairingId)}
        >
          <CloseIcon className="stroke" />
        </button>
      </div>
    </div>
  );
}

/** The code as one tile per digit, in two groups of three. */
function Code({ code }: { code: string }) {
  const groups = [code.slice(0, 3), code.slice(3)];
  return (
    <div
      className="code"
      role="img"
      aria-label={`Pairing code ${groups.join(" ")}`}
      title="Enter this code on the device"
    >
      {[...code].map((digit, index) => (
        <span key={index} className={index === 3 ? "digit after-gap" : "digit"}>
          {digit}
        </span>
      ))}
    </div>
  );
}

/** The current time, updated once a second. */
function useClock(): number {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  return now;
}
