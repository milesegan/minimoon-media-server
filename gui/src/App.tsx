import { useEffect } from "react";
import { selectServer, useStore } from "./store";
import { HeaderBand } from "./components/band/HeaderBand";
import { Banners } from "./components/Banners";
import { PairingRequests } from "./components/pairing/PairingRequests";
import { FoldersSection } from "./components/folders/FoldersSection";
import { DevicesSection } from "./components/devices/DevicesSection";
import { ServerSection } from "./components/settings/ServerSection";
import { Footer } from "./components/Footer";
import { Toast } from "./components/Toast";

/** How often the status is fetched, in milliseconds. */
const POLL_INTERVAL = 1000;

/**
 * The whole window, one column: the band and anything that needs attention, then the settings
 * once the server runs (until then the band and banners say why there are none), then the footer.
 */
export default function App() {
  const server = useStore(selectServer);
  usePolling();
  useSuppressedWebviewMenu();
  useTitle(server ? `${server.name} – MiniMoon Media Server` : null);
  useTransitionsAfterFirstRender(server !== null);

  return (
    <>
      <div className="page">
        <div className="top">
          <HeaderBand />
          <Banners />
          <PairingRequests />
        </div>
        {server && (
          <main>
            <FoldersSection folders={server.folders} />
            <DevicesSection server={server} />
            <ServerSection server={server} />
          </main>
        )}
        <Footer />
      </div>
      <Toast />
    </>
  );
}

/** Fetches the status now and then once a second. */
function usePolling() {
  const refresh = useStore((state) => state.refresh);
  useEffect(() => {
    refresh();
    const timer = setInterval(refresh, POLL_INTERVAL);
    return () => clearInterval(timer);
  }, [refresh]);
}

/** Keeps the webview's own context menu (Reload, Inspect) away, except where text is selectable. */
function useSuppressedWebviewMenu() {
  useEffect(() => {
    const suppress = (event: MouseEvent) => {
      const target = event.target as Element;
      if (
        !event.defaultPrevented &&
        !target.closest("input, .selectable, .row-sub, details ul")
      ) {
        event.preventDefault();
      }
    };
    document.addEventListener("contextmenu", suppress);
    return () => document.removeEventListener("contextmenu", suppress);
  }, []);
}

function useTitle(title: string | null) {
  useEffect(() => {
    if (title) document.title = title;
  }, [title]);
}

/**
 * Turns on CSS transitions once the settings have rendered, so switches don't animate from
 * their initial state into the saved one.
 */
function useTransitionsAfterFirstRender(rendered: boolean) {
  useEffect(() => {
    if (!rendered) return;
    const frame = requestAnimationFrame(() =>
      document.body.classList.add("loaded"),
    );
    return () => cancelAnimationFrame(frame);
  }, [rendered]);
}
