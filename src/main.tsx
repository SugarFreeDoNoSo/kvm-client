import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import MobileApp from "./mobile/MobileApp";
import "./styles.css";

const mobile = /Android/i.test(navigator.userAgent) || new URLSearchParams(location.search).has("android-preview");

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {mobile ? <MobileApp /> : <App />}
  </React.StrictMode>,
);
