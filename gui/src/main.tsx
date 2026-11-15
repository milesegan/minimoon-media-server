import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";

// In cascade order: tokens and base rules first, then each part of the window.
import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/controls.css";
import "./styles/band.css";
import "./styles/banners.css";
import "./styles/pairing.css";
import "./styles/sections.css";
import "./styles/folders.css";
import "./styles/settings.css";
import "./styles/footer.css";
import "./styles/toast.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
