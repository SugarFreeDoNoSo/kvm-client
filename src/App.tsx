import { useEffect, useRef, useState, type FormEvent } from "react";
import {
  call, errorMessage, nativeAvailable, onEvent,
  type AppStatus, type DeviceInspection, type InputSettings, type Metrics, type Preferences, type PreferencesPatch, type VideoSettings,
} from "./backend";

import SunshineControls from "./SunshineControls";

const defaultAddress = "https://192.168.100.8/";
const defaultVideo: VideoSettings = {
  filter: "desktop", sharpness: 0.2, nativeSize: false, scaleFactor: 0,
  antiRinging: 1, edgeThreshold: 8 / 255, downscaleAntiAliasing: true,
};
const isMac = /Mac/.test(navigator.platform);
const initialStatus: AppStatus = {
  state: "disconnected", message: "Listo para conectar", address: defaultAddress,
  phase: "idle", videoState: "waiting", videoPaused: false, inputEnabled: true,
  capture: false, captureRequested: false, inputBackend: "", decoder: "",
};

function Icon({ name, size = 16 }: { name: "connect" | "expand" | "mouse" | "shield" | "close" | "chevron" | "panel" | "pause" | "play"; size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      {name === "chevron" && <path d="m6 14 6-6 6 6" />}
      {name === "pause" && <><path d="M8 5v14m8-14v14" strokeWidth="3" /></>}
      {name === "play" && <path d="m8 4 12 8-12 8V4Z" />}
      {name === "panel" && <><rect x="3" y="3" width="18" height="18" /><path d="M9 3v18m5-12h4m-4 6h4" /></>}
      {name === "connect" && <><path d="M8 3v5m8-5v5M6 8h12v3a6 6 0 0 1-12 0V8Zm6 9v4" /></>}
      {name === "expand" && <path d="M8 3H3v5m13-5h5v5M3 16v5h5m13-5v5h-5" />}
      {name === "mouse" && <><rect x="6" y="2" width="12" height="20" rx="6" /><path d="M12 2v7" /></>}
      {name === "shield" && <><path d="m12 2 8 3v6c0 5-8 11-8 11S4 16 4 11V5l8-3Z" /><path d="m8 12 3 3 5-6" /></>}
      {name === "close" && <path d="m6 6 12 12M6 18 18 6" />}
    </svg>
  );
}

function metric(value: number | null | undefined, precision = 0): string {
  return value == null || !Number.isFinite(value) ? "—" : value.toFixed(precision);
}

function savedPasswordOrigin(address: string): string | null {
  try {
    const url = new URL(address.trim());
    if (url.protocol !== "https:" || url.username || url.password || url.pathname !== "/" || url.search || url.hash) return null;
    return `${url.origin}/`;
  } catch { return null; }
}

function VideoAdjustment({ label, value, min, max, unit = "", disabled, hint, onCommit, onError }: {
  label: string; value: number; min: number; max: number; unit?: string; disabled: boolean;
  hint: string; onCommit: (value: number) => void; onError: (message: string) => void;
}) {
  const [draft, setDraft] = useState(String(value));
  useEffect(() => setDraft(String(value)), [value]);
  function commit(raw: string) {
    const next = Number(raw);
    if (!raw.trim() || !Number.isInteger(next) || next < min || next > max) {
      setDraft(String(value));
      onError(`${label}: introduce un entero entre ${min} y ${max}${unit ? ` ${unit}` : ""}.`);
      return;
    }
    setDraft(String(next));
    if (next !== value) onCommit(next);
  }
  const rangeValue = draft.trim() && Number.isFinite(Number(draft)) ? Math.min(max, Math.max(min, Number(draft))) : value;
  return (
    <div className="video-adjustment">
      <div className="video-adjustment-heading"><span>{label}</span><label className="video-adjustment-value"><input type="number" min={min} max={max} step={1} value={draft} onChange={(event) => setDraft(event.target.value)} onBlur={(event) => commit(event.currentTarget.value)} onKeyDown={(event) => { if (event.key === "Enter") event.currentTarget.blur(); }} disabled={disabled} aria-label={label} />{unit && <span>{unit}</span>}</label></div>
      <input type="range" min={min} max={max} step={1} value={rangeValue} onChange={(event) => setDraft(event.target.value)} onPointerUp={(event) => commit(event.currentTarget.value)} onKeyUp={(event) => { if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End", "PageUp", "PageDown"].includes(event.key)) commit(event.currentTarget.value); }} onBlur={(event) => commit(event.currentTarget.value)} disabled={disabled} aria-label={`${label}: deslizador`} />
      <p>{hint}</p>
    </div>
  );
}

export default function App() {
  const [address, setAddress] = useState(defaultAddress);
  const [password, setPassword] = useState("");
  const [status, setStatus] = useState<AppStatus>(initialStatus);
  const [metrics, setMetrics] = useState<Metrics | null>(null);
  const [input, setInput] = useState<InputSettings>({ hz: 125, sensitivity: 1 });
  const [sensitivityDraft, setSensitivityDraft] = useState("1");
  const [video, setVideo] = useState<VideoSettings>(defaultVideo);
  const [busy, setBusy] = useState(false);
  const [inputSettingsBusy, setInputSettingsBusy] = useState(false);
  const [videoPending, setVideoPending] = useState(false);
  const [error, setError] = useState("");
  const [inspection, setInspection] = useState<DeviceInspection | null>(null);
  const [fullscreen, setFullscreen] = useState(false);
  const [dashboardExpanded, setDashboardExpanded] = useState(false);
  const [dashboardFullscreen, setDashboardFullscreen] = useState(false);
  const [dashboardModeUpdating, setDashboardModeUpdating] = useState(false);
  const [dashboardUpdating, setDashboardUpdating] = useState(false);
  const [preferencesReady, setPreferencesReady] = useState(!nativeAvailable);
  const [recentAddresses, setRecentAddresses] = useState<string[]>([]);
  const [rememberPassword, setRememberPassword] = useState(true);
  const [hasSavedPassword, setHasSavedPassword] = useState(false);
  const [passwordLoading, setPasswordLoading] = useState(false);
  const [persistenceNotice, setPersistenceNotice] = useState("");
  const active = status.state === "connected";
  const connecting = status.state === "connecting";
  const settingsBusy = inputSettingsBusy || videoPending;
  const locked = busy || connecting || settingsBusy || !preferencesReady;
  const videoLocked = !nativeAvailable || busy || connecting || inputSettingsBusy || !preferencesReady;
  const mounted = useRef(true);
  const statusRevision = useRef(0);
  const operationInFlight = useRef(false);
  const videoRef = useRef<VideoSettings>(defaultVideo);
  const appliedVideo = useRef<VideoSettings>(defaultVideo);
  const videoQueue = useRef<Promise<void>>(Promise.resolve());
  const videoRevision = useRef(0);
  const videoUpdates = useRef(0);
  const wasCaptured = useRef(false);
  const passwordRevision = useRef(0);
  const passwordTouched = useRef(false);
  const dashboardRevision = useRef(0);
  const dashboardExpandedRef = useRef(false);
  const preferencesReadyRef = useRef(!nativeAvailable);
  const dashboardRequest = useRef(false);
  const dashboardModeRequest = useRef(false);
  const addressInput = useRef<HTMLInputElement>(null);
  const dashboardToggle = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (fullscreen && !dashboardFullscreen && wasCaptured.current && !status.capture) {
      void call("set_fullscreen", { enabled: false }).then(() => setFullscreen(false)).catch((reason) => setError(errorMessage(reason)));
    }
    wasCaptured.current = status.capture;
  }, [fullscreen, dashboardFullscreen, status.capture]);

  useEffect(() => {
    mounted.current = true;
    if (!nativeAvailable) return () => { mounted.current = false; };
    const unsubscribe: Array<() => void> = [];
    let cancelled = false;
    let receivedStatus = false;
    void onEvent<AppStatus>("kvm://status", (next) => {
      if (cancelled) return;
      receivedStatus = true;
      statusRevision.current += 1;
      setStatus(next);
      setError("");
      if (next.state === "disconnected" || next.state === "error") setMetrics(null);
    }).then((stop) => cancelled ? stop() : unsubscribe.push(stop)).catch((reason) => {
      if (!cancelled) setError(errorMessage(reason));
    });
    void onEvent<boolean>("kvm://fullscreen", (enabled) => {
      if (!cancelled) setFullscreen(enabled);
    }).then((stop) => cancelled ? stop() : unsubscribe.push(stop)).catch((reason) => {
      if (!cancelled) setError(errorMessage(reason));
    });
    void onEvent<string>("kvm://credential-error", (message) => {
      if (!cancelled) setPersistenceNotice(message);
    }).then((stop) => cancelled ? stop() : unsubscribe.push(stop)).catch((reason) => {
      if (!cancelled) setPersistenceNotice(errorMessage(reason));
    });
    void onEvent<boolean>("kvm://dashboard-fullscreen", (enabled) => {
      if (!cancelled) setDashboardFullscreen(enabled);
    }).then((stop) => cancelled ? stop() : unsubscribe.push(stop)).catch((reason) => {
      if (!cancelled) setError(errorMessage(reason));
    });
    void onEvent<boolean>("kvm://dashboard", (expanded) => {
      if (cancelled) return;
      dashboardRevision.current += 1;
      dashboardExpandedRef.current = expanded;
      setDashboardExpanded(expanded);
      if (!expanded) setDashboardFullscreen(false);
      if (preferencesReadyRef.current) {
        void call<Preferences>("save_preferences", { patch: { dashboardExpanded: expanded } })
          .catch((reason) => !cancelled && setPersistenceNotice(`No se pudo guardar el estado del panel: ${errorMessage(reason)}`));
      }
    }).then((stop) => cancelled ? stop() : unsubscribe.push(stop)).catch((reason) => {
      if (!cancelled) setError(errorMessage(reason));
    });
    void onEvent<Metrics>("kvm://metrics", (next) => {
      if (!cancelled) setMetrics(next);
    }).then((stop) => cancelled ? stop() : unsubscribe.push(stop)).catch((reason) => {
      if (!cancelled) setError(errorMessage(reason));
    });
    void call<AppStatus>("get_status").then((next) => {
      if (!cancelled && !receivedStatus) setStatus(next);
    }).catch((reason) => {
      if (!cancelled) setError(errorMessage(reason));
    });
    return () => {
      cancelled = true;
      mounted.current = false;
      unsubscribe.forEach((stop) => stop());
    };
  }, []);

  useEffect(() => {
    if (!nativeAvailable) return;
    let cancelled = false;
    const revision = dashboardRevision.current;
    void call<Preferences>("get_preferences").then(async (saved) => {
      if (cancelled) return;
      setAddress(saved.address);
      setRecentAddresses(saved.recentAddresses);
      setInput(saved.input);
      setSensitivityDraft(String(saved.input.sensitivity));
      const restoredVideo = { ...defaultVideo, ...saved.video };
      videoRef.current = restoredVideo;
      appliedVideo.current = restoredVideo;
      setVideo(restoredVideo);
      setRememberPassword(saved.rememberPassword);
      if (dashboardRevision.current === revision) {
        await call("set_dashboard_expanded", { expanded: saved.dashboardExpanded });
        if (!cancelled && dashboardRevision.current === revision) {
          dashboardExpandedRef.current = saved.dashboardExpanded;
          setDashboardExpanded(saved.dashboardExpanded);
        }
      }
    }).catch((reason) => {
      if (!cancelled) setPersistenceNotice(`No se pudieron recuperar los ajustes: ${errorMessage(reason)}`);
    }).finally(() => {
      if (!cancelled) {
        preferencesReadyRef.current = true;
        setPreferencesReady(true);
      }
    });
    return () => { cancelled = true; };
  }, []);

  useEffect(() => {
    if (!nativeAvailable || !preferencesReady || active || !rememberPassword || passwordTouched.current) {
      setPasswordLoading(false);
      return;
    }
    let cancelled = false;
    const revision = ++passwordRevision.current;
    const origin = savedPasswordOrigin(address);
    setHasSavedPassword(false);
    if (!origin) { setPasswordLoading(false); return; }
    setPasswordLoading(true);
    void call<string | null>("load_saved_password", { address: origin }).then((saved) => {
      if (cancelled || passwordRevision.current !== revision) return;
      setPassword(saved ?? "");
      setHasSavedPassword(saved !== null);
    }).catch((reason) => {
      if (!cancelled && passwordRevision.current === revision) setPersistenceNotice(`No se pudo recuperar la contraseña: ${errorMessage(reason)}`);
    }).finally(() => {
      if (!cancelled && passwordRevision.current === revision) setPasswordLoading(false);
    });
    return () => { cancelled = true; };
  }, [address, active, preferencesReady, rememberPassword]);

  async function persistPreferences(patch: PreferencesPatch) {
    if (!nativeAvailable || !preferencesReady) return;
    try {
      const saved = await call<Preferences>("save_preferences", { patch });
      if (mounted.current) setRecentAddresses(saved.recentAddresses);
    } catch (reason) {
      if (mounted.current) setPersistenceNotice(`Los cambios están activos, pero no se pudieron guardar: ${errorMessage(reason)}`);
    }
  }

  async function setDashboardOpen(expanded: boolean) {
    if (dashboardRequest.current || !preferencesReady) return;
    dashboardRequest.current = true;
    setDashboardUpdating(true);
    try {
      if (nativeAvailable) await call("set_dashboard_expanded", { expanded });
      dashboardRevision.current += 1;
      dashboardExpandedRef.current = expanded;
      setDashboardExpanded(expanded);
      if (!expanded) setDashboardFullscreen(false);
      if (expanded) requestAnimationFrame(() => dashboardToggle.current?.focus());
      await persistPreferences({ dashboardExpanded: expanded });
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      dashboardRequest.current = false;
      if (mounted.current) setDashboardUpdating(false);
    }
  }

  async function setDashboardFull(enabled: boolean) {
    if (dashboardModeRequest.current || !preferencesReady) return;
    dashboardModeRequest.current = true;
    setDashboardModeUpdating(true);
    try {
      if (nativeAvailable) await call("set_dashboard_fullscreen", { enabled });
      setDashboardFullscreen(enabled);
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      dashboardModeRequest.current = false;
      if (mounted.current) setDashboardModeUpdating(false);
    }
  }

  useEffect(() => {
    if (!dashboardFullscreen) return;
    const onEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented || (event.target instanceof Element && event.target.closest('[role="dialog"]'))) return;
      event.preventDefault();
      void setDashboardFull(false);
    };
    window.addEventListener("keydown", onEscape);
    return () => window.removeEventListener("keydown", onEscape);
  }, [dashboardFullscreen, preferencesReady]);

  async function ensureDashboardExpanded() {
    if (!dashboardExpanded) await setDashboardOpen(true);
  }

  function changeAddress(next: string) {
    passwordRevision.current += 1;
    passwordTouched.current = false;
    setPasswordLoading(false);
    setAddress(next);
    setPassword("");
    setHasSavedPassword(false);
  }

  async function forgetPassword() {
    const origin = savedPasswordOrigin(address);
    if (!origin) return;
    passwordRevision.current += 1;
    passwordTouched.current = true;
    setPasswordLoading(false);
    setPassword("");
    try {
      await call("delete_saved_password", { address: origin });
      setHasSavedPassword(false);
      setPersistenceNotice("Contraseña eliminada del almacén seguro.");
    } catch (reason) {
      setPersistenceNotice(`No se pudo eliminar la contraseña guardada: ${errorMessage(reason)}`);
    }
  }

  async function refreshStatus() {
    const revision = statusRevision.current;
    const next = await call<AppStatus>("get_status");
    if (mounted.current && statusRevision.current === revision) setStatus(next);
  }

  async function connect(verifiedAddress = address) {
    await call("set_input_settings", { settings: input });
    await call("set_video_settings", { settings: video });
    let authenticatedPassword = password;
    passwordRevision.current += 1;
    if (rememberPassword && !authenticatedPassword && !passwordTouched.current) {
      try { authenticatedPassword = await call<string | null>("load_saved_password", { address: verifiedAddress }) ?? ""; }
      catch (reason) { setPersistenceNotice(`No se pudo recuperar la contraseña: ${errorMessage(reason)}`); }
    }
    setPasswordLoading(false);
    await call("connect_device", { address: verifiedAddress, password: authenticatedPassword, rememberPassword });
    passwordTouched.current = false;
    setPassword("");
    await refreshStatus();
    await persistPreferences({ address: verifiedAddress, rememberPassword });

  }

  async function onConnect(event: FormEvent) {
    event.preventDefault();
    if (!address.trim() || locked || passwordLoading || operationInFlight.current || !nativeAvailable) return;
    operationInFlight.current = true;
    setError("");
    setBusy(true);
    setPersistenceNotice("");
    try {
      const device = await call<DeviceInspection>("inspect_device", { address: address.trim() });
      if (device.requiresTrust && !device.trusted) {
        setInspection(device);
      } else {
        await connect(device.address);
      }
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      operationInFlight.current = false;
      setBusy(false);
    }
  }

  async function acceptCertificate() {
    if (!inspection || operationInFlight.current) return;
    operationInFlight.current = true;
    setBusy(true);
    setError("");
    try {
      await call("trust_device", { address: inspection.address, fingerprint: inspection.fingerprint });
      const verifiedAddress = inspection.address;
      setInspection(null);
      await connect(verifiedAddress);
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      operationInFlight.current = false;
      setBusy(false);
    }
  }

  async function action(command: string, args?: Record<string, unknown>) {
    if (operationInFlight.current) return;
    operationInFlight.current = true;
    setBusy(true);
    setError("");
    try {
      await call(command, args);
      await refreshStatus();
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      operationInFlight.current = false;
      setBusy(false);
    }
  }

  async function updateInput(next: InputSettings) {
    if (operationInFlight.current) return;
    operationInFlight.current = true;
    setInputSettingsBusy(true);
    setError("");
    try {
      await call("set_input_settings", { settings: next });
      setInput(next);
      await persistPreferences({ input: next });
    } catch (reason) {
      setSensitivityDraft(String(input.sensitivity));
      setError(errorMessage(reason));
    } finally {
      operationInFlight.current = false;
      setInputSettingsBusy(false);
    }
  }

  async function updateVideo(patch: Partial<VideoSettings>) {
    const next = { ...videoRef.current, ...patch };
    if ((Object.keys(next) as Array<keyof VideoSettings>).every((key) => next[key] === videoRef.current[key])) return;
    const revision = ++videoRevision.current;
    videoRef.current = next;
    setVideo(next);
    videoUpdates.current += 1;
    operationInFlight.current = true;
    setVideoPending(true);
    setError("");
    const update = videoQueue.current.then(async () => {
      try {
        await call("set_video_settings", { settings: next });
        appliedVideo.current = next;
        await persistPreferences({ video: next });
      } catch (reason) {
        if (mounted.current) {
          if (revision === videoRevision.current) {
            videoRef.current = appliedVideo.current;
            setVideo(appliedVideo.current);
          }
          setError(errorMessage(reason));
        }
      } finally {
        videoUpdates.current -= 1;
        if (videoUpdates.current === 0) {
          operationInFlight.current = false;
          if (mounted.current) setVideoPending(false);
        }
      }
    });
    videoQueue.current = update;
    await update;
  }

  async function toggleFullscreen() {
    setError("");
    try {
      await call("set_fullscreen", { enabled: !fullscreen });
      setFullscreen(!fullscreen);
    } catch (reason) {
      setError(errorMessage(reason));
    }
  }

  const nativeError = status.state === "error";
  const statusText = !nativeAvailable ? "Vista previa · abre la aplicación de escritorio para conectar" : nativeError ? status.message : error || persistenceNotice || status.message || "Listo para conectar";
  const phaseText = { idle: "", tls: "TLS", auth: "Autenticación", signaling: "Señalización", webrtc: "WebRTC", ready: nativeError ? "Sesión interrumpida" : "Canal conectado" }[status.phase];
  const videoText = active ? status.videoPaused ? "Video KVM pausado" : { waiting: "Esperando video", playing: "Video activo", no_signal: "Sin señal HDMI", stalled: "Video detenido" }[status.videoState] : "";
  const resolutionText = metrics && metrics.inputWidth > 0 && metrics.inputHeight > 0 && metrics.outputWidth > 0 && metrics.outputHeight > 0
    ? `${metrics.inputWidth}×${metrics.inputHeight} → ${metrics.outputWidth}×${metrics.outputHeight}` : "";
  const captureHint = isMac ? "macOS · captura en esta ventana; Ctrl+Option+Esc libera" : /X11|XInput/.test(status.inputBackend) ? "X11 · captura en la ventana enfocada; Ctrl+Alt+Esc libera" : "Wayland: autoriza el portal y cruza el borde derecho; Ctrl+Alt+Esc libera";
  const adjustableVideo = video.filter === "desktop" || video.filter === "gaming";
  const scaleValue = video.nativeSize ? "native" : video.scaleFactor > 0 ? String(video.scaleFactor) : "fit";
  const profileHint = {
    original: "Bilineal de referencia, sin enfoque adicional.",
    desktop: "Bicúbico para conservar texto y formas, con control de halos.",
    gaming: "SGSR v1 espacial para reconstruir contornos al ampliar juegos 3D.",
    nearest: "Vecino más cercano: conserva bordes duros y píxeles definidos.",
  }[video.filter];

  return (
    <>
      <main className={`console dashboard ${dashboardExpanded ? "is-expanded" : "is-collapsed"} ${dashboardFullscreen ? "is-fullscreen" : ""}`} aria-label="Consola IPKVM" hidden={!dashboardExpanded}>
        <header className="dashboard-bar">
          <button ref={dashboardToggle} type="button" className="dashboard-toggle" aria-label="Ocultar panel lateral" aria-expanded={dashboardExpanded} aria-controls="dashboard-details" disabled={dashboardUpdating || dashboardModeUpdating || !preferencesReady} onClick={() => void setDashboardOpen(!dashboardExpanded)}>
            <span className="dashboard-chevron"><Icon name="close" /></span>
            <span><strong>Panel de control</strong><small>Conexión y métricas</small></span>
          </button>
          <button className={dashboardFullscreen ? "button secondary panel-mode-button" : "icon-button"} type="button" title={dashboardFullscreen ? "Volver al panel lateral" : "Pantalla completa del panel"} aria-label={dashboardFullscreen ? "Volver al lateral" : "Pantalla completa del panel"} aria-pressed={dashboardFullscreen} disabled={dashboardModeUpdating || dashboardUpdating || !preferencesReady} onClick={() => void setDashboardFull(!dashboardFullscreen)}>{dashboardFullscreen ? "Volver al lateral" : <Icon name="panel" />}</button>
          <button className="icon-button" type="button" title={fullscreen ? "Salir de pantalla completa del video" : "Pantalla completa del video · oculta el panel; libera el control para volver"} aria-label={fullscreen ? "Salir de pantalla completa del video" : "Pantalla completa del video"} onClick={() => void toggleFullscreen()} disabled={!nativeAvailable}><Icon name="expand" /></button>
        </header>
        <div id="dashboard-details" className="dashboard-details" hidden={!dashboardExpanded}>
          <section className="sidebar-session" aria-label="Estado de conexión">
            <div className="session-state" data-state={nativeAvailable ? status.state : "disconnected"}>
              <span className={`state-dot ${connecting ? "pulse" : ""}`} />
              {!nativeAvailable ? "VISTA PREVIA" : active ? "EN LÍNEA" : connecting ? "CONECTANDO" : status.state === "error" ? "SIN CONEXIÓN" : "DESCONECTADO"}
            </div>
            <div className="dashboard-summary" title={statusText}>
              <span>{active ? `${metric(metrics?.presentedFps)} FPS` : statusText}</span>
              {active && <span>{metric(metrics?.rttMs, 1)} ms RTT</span>}
            </div>
          </section>
          <div className="sidebar-source">
            <SunshineControls kvmConnected={active} onConfigure={ensureDashboardExpanded} />
            <button type="button" className={`button kvm-video-pause ${status.videoPaused ? "is-paused" : ""}`} aria-pressed={status.videoPaused} title="Mantiene teclado y mouse; detiene la decodificación y presentación local del KVM. No pausa Sunshine." onClick={() => void action("set_kvm_video_paused", { paused: !status.videoPaused })} disabled={!active || busy || settingsBusy}>
              <Icon name={status.videoPaused ? "play" : "pause"} />{status.videoPaused ? "Reanudar video KVM" : "Pausar video KVM"}
            </button>
            <button type="button" className={`button kvm-input-toggle ${!status.inputEnabled ? "is-disabled" : ""}`} aria-pressed={!status.inputEnabled} title="Al desactivar, libera teclas y botones y detiene el envío de entrada al KVM. El video y Sunshine continúan. Al activar, captura el control manualmente." onClick={() => void action("set_kvm_input_enabled", { enabled: !status.inputEnabled })} disabled={!nativeAvailable || busy || settingsBusy || connecting || !preferencesReady}>
              <Icon name="mouse" />{status.inputEnabled ? "Desactivar teclado y mouse KVM" : "Activar teclado y mouse KVM"}
            </button>
          </div>
          <div className="dashboard-section-heading"><span>CONEXIÓN Y RENDIMIENTO</span><span className="shortcut"><kbd>Ctrl</kbd><kbd>{isMac ? "Option" : "Alt"}</kbd><kbd>Esc</kbd><span>liberar control</span></span></div>
        <form className="connection-row" onSubmit={(event) => void onConnect(event)}>
          <label className="field address-field"><span>DIRECCIÓN DEL KVM</span><div className="address-input"><span className="protocol-dot" /><input ref={addressInput} list="recent-kvm-addresses" aria-label="Dirección del KVM" type="url" value={address} onChange={(event) => changeAddress(event.target.value)} placeholder={defaultAddress} spellCheck={false} autoCapitalize="none" autoCorrect="off" required disabled={active || locked} /></div></label>
          <datalist id="recent-kvm-addresses">{recentAddresses.map((recent) => <option key={recent} value={recent} />)}</datalist>
          <label className="field password-field"><span>CONTRASEÑA LOCAL</span><input aria-label="Contraseña local" type="password" value={password} onChange={(event) => { passwordRevision.current += 1; passwordTouched.current = true; setPasswordLoading(false); setPassword(event.target.value); }} placeholder={passwordLoading ? "Recuperando contraseña…" : "Si el dispositivo la requiere"} autoComplete="off" disabled={active || locked} /></label>
          <div className="credential-options">
            <label title="Guardar la contraseña en el almacén seguro del sistema"><input type="checkbox" checked={rememberPassword} onChange={(event) => { const remember = event.target.checked; passwordRevision.current += 1; setPasswordLoading(false); setRememberPassword(remember); void persistPreferences({ rememberPassword: remember }); }} disabled={active || locked} />Recordar</label>
            {hasSavedPassword && <button type="button" onClick={() => void forgetPassword()} disabled={active || locked}>Olvidar contraseña</button>}
          </div>
          {active || connecting ? (
            <button type="button" className="button disconnect" onClick={() => void action("disconnect_device")} disabled={busy || settingsBusy}><Icon name="connect" />Desconectar</button>
          ) : (
            <button type="submit" className="button primary" disabled={locked || passwordLoading || !nativeAvailable || !address.trim()}><Icon name="connect" />{passwordLoading ? "Cargando contraseña…" : busy ? "Verificando…" : "Conectar"}</button>
          )}
          <button type="button" className={`button capture ${status.capture ? "captured" : ""}`} title={status.inputEnabled ? captureHint : "Activa el teclado y mouse KVM antes de capturar el control."} onClick={() => void action("set_capture", { enabled: !status.captureRequested })} disabled={!active || busy || settingsBusy || !status.inputEnabled}><Icon name="mouse" />{status.capture ? "Liberar control" : status.captureRequested ? "Cancelar captura" : "Capturar control"}</button>
        </form>

        <section className="settings-row" aria-label="Ajustes de transmisión">
          <div className="setting-group mouse-group">
            <span className="group-label">MOUSE <span>{status.inputEnabled ? "RELATIVO" : "DESACTIVADO"}</span></span>
            <label className="inline-setting"><span>Envío</span><select value={input.hz} onChange={(event) => void updateInput({ ...input, hz: Number(event.target.value) })} disabled={!nativeAvailable || settingsBusy || busy} aria-label="Frecuencia de envío del mouse">{[125, 250, 500, 1000].map((hz) => <option key={hz} value={hz}>{hz} Hz</option>)}</select></label>
            <label className="inline-setting sensitivity"><span>Sensibilidad</span><input type="number" value={sensitivityDraft} min={0.1} max={4} step={0.1} onChange={(event) => setSensitivityDraft(event.target.value)} onBlur={() => {
              const next = Number(sensitivityDraft);
              if (Number.isFinite(next) && next >= 0.1 && next <= 4) {
                if (next !== input.sensitivity) void updateInput({ ...input, sensitivity: next });
              } else {
                setSensitivityDraft(String(input.sensitivity));
                setError("La sensibilidad debe estar entre 0,1 y 4.");
              }
            }} onKeyDown={(event) => { if (event.key === "Enter") event.currentTarget.blur(); }} disabled={!nativeAvailable || settingsBusy || busy} aria-label="Sensibilidad del mouse" /><span className="unit">×</span></label>
          </div>
          <div className="setting-group video-group">
            <span className="group-label">IMAGEN KVM <span>{videoPending ? "GUARDANDO…" : "REESCALADO"}</span></span>
            <label className="inline-setting video-profile"><span>Perfil</span><select value={video.filter} onChange={(event) => void updateVideo({ filter: event.target.value as VideoSettings["filter"] })} disabled={videoLocked} aria-label="Perfil de imagen">
              <option value="original">Original</option>
              <option value="desktop">Escritorio</option>
              <option value="gaming">Juegos 3D</option>
              <option value="nearest">Píxeles</option>
            </select></label>
            <p className="video-help">{profileHint}</p>
            <label className="inline-setting video-scale"><span>Presentación</span><select value={scaleValue} onChange={(event) => { const value = event.target.value; void updateVideo({ nativeSize: value === "native", scaleFactor: value === "fit" || value === "native" ? 0 : Number(value) }); }} disabled={videoLocked} aria-label="Escala de presentación">
              <option value="fit">Ajustar</option>
              <option value="native">1:1</option>
              <option value="2">2×</option>
              <option value="3">3×</option>
              <option value="4">4×</option>
            </select></label>
            <p className="video-help">{scaleValue === "fit" ? "Ajusta al visor y conserva las proporciones." : scaleValue === "native" ? "Un píxel recibido por píxel físico. Si no cabe, recorta el centro." : `Amplía cada dimensión ${scaleValue} veces. Si no cabe, recorta el centro.`}</p>
            {adjustableVideo && <VideoAdjustment label="Nitidez" value={Math.round(video.sharpness * 100)} min={0} max={100} unit="%" disabled={videoLocked} onCommit={(value) => void updateVideo({ sharpness: value / 100 })} onError={setError} hint={video.filter === "gaming" ? "0 % conserva la reconstrucción SGSR con enfoque mínimo." : "0 % desactiva el enfoque adicional; sube suavemente para texto."} />}
            {adjustableVideo && <details className="video-advanced">
              <summary>Ajustes avanzados</summary>
              <div className="video-advanced-content">
                {video.filter === "desktop" && <VideoAdjustment label="Control de halos" value={Math.round(video.antiRinging * 100)} min={0} max={100} unit="%" disabled={videoLocked} onCommit={(value) => void updateVideo({ antiRinging: value / 100 })} onError={setError} hint="100 % limita los halos del bicúbico alrededor de letras y bordes." />}
                {video.filter === "gaming" && <VideoAdjustment label="Umbral de bordes" value={Math.round(video.edgeThreshold * 255)} min={1} max={32} disabled={videoLocked} onCommit={(value) => void updateVideo({ edgeThreshold: value / 255 })} onError={setError} hint="Bajo: detecta más detalles y ruido. Alto: prioriza los contornos fuertes." />}
                <label className="video-check"><input type="checkbox" checked={video.downscaleAntiAliasing} disabled={videoLocked} onChange={(event) => void updateVideo({ downscaleAntiAliasing: event.target.checked })} /><span>Suavizar al reducir<small>Reduce bordes dentados cuando la imagen recibida es mayor que el visor.</small></span></label>
                <button type="button" className="button secondary video-reset" disabled={videoLocked} onClick={() => void updateVideo({ sharpness: defaultVideo.sharpness, antiRinging: defaultVideo.antiRinging, edgeThreshold: defaultVideo.edgeThreshold, downscaleAntiAliasing: defaultVideo.downscaleAntiAliasing })}>Restablecer calidad</button>
                <p className="video-help">Conserva el perfil y la escala de presentación.</p>
              </div>
            </details>}
          </div>
        </section>

        <div className="sidebar-section-title">RENDIMIENTO</div>
        <section className="telemetry-row" aria-label="Métricas en vivo">
          <div className="metric"><span>VIDEO <small>recibido / decodificado / presentado</small></span><strong>{metric(metrics?.receivedFps)} <i>/</i> {metric(metrics?.decodedFps)} <i>/</i> {metric(metrics?.presentedFps)}<em>FPS</em></strong></div>
          <div className="metric"><span>MOUSE <small>efectivo / solicitado</small></span><strong>{metric(metrics?.inputHz)} <i>/</i> {input.hz}<em>Hz</em></strong></div>
          <div className="metric"><span>RTT</span><strong>{metric(metrics?.rttMs, 1)}<em>ms</em></strong></div>
          <div className="metric"><span>PÉRDIDAS</span><strong>{metric(metrics?.packetsLost)}</strong></div>
          <div className={`metric queue-metric ${metrics?.congested ? "warning" : ""}`}><span>COLA</span><strong>{metric(metrics?.queueDepth)}{metrics?.congested && <em>congestión</em>}</strong></div>
          <div className="engine-info"><span title={captureHint}>{status.inputBackend || (isMac ? "Entrada nativa · macOS" : "Entrada nativa")}</span><span className="video-state" data-state={active ? status.videoPaused ? "paused" : status.videoState : "waiting"} title={[videoText, status.decoder, "Resolución recibida → resolución procesada, en píxeles físicos"].filter(Boolean).join(" · ")}>{resolutionText || videoText || status.decoder || "Esperando dimensiones de video"}</span><span title="Tiempo de pared del filtro, incluidas sus esperas; no es un temporizador de GPU ni latencia entre los equipos. Descartes acumulados del visor.">Filtro {metric(metrics?.filterProcessingMs, 2)} ms · desc. {metric(metrics?.sinkDroppedFrames)}</span></div>
        </section>

        <footer className={`statusbar ${error || status.state === "error" ? "has-error" : ""}`}>
          <span className="status-message" role="status" title={statusText}><span className="status-square" />{statusText}</span>
          <span className="status-note">{!status.inputEnabled ? "ENTRADA KVM DESACTIVADA" : status.capture ? "CONTROL CAPTURADO" : phaseText || "1:1 · SIN PREDICCIÓN"}</span>
          {error && !nativeError && <button type="button" className="dismiss-error" onClick={() => setError("")} aria-label="Cerrar mensaje de error"><Icon name="close" size={12} /></button>}
        </footer>
        </div>
      </main>

      {!nativeAvailable && !dashboardExpanded && <button type="button" className="browser-dashboard-reveal" onClick={() => void setDashboardOpen(true)} aria-label="Mostrar panel lateral" aria-expanded={false} aria-controls="dashboard-details"><Icon name="chevron" />Mostrar panel</button>}
      {!nativeAvailable && <div className={`browser-placeholder ${dashboardExpanded ? "dashboard-open" : ""} ${dashboardFullscreen ? "dashboard-fullscreen" : ""}`}><div className="placeholder-crosshair" aria-hidden="true">+</div><h1>Tu pantalla.<br />Tu control.</h1><p>Abre la consola de escritorio para conectar el KVM.</p><code>npm run desktop</code><span>Esta vista previa no abre conexiones al dispositivo.</span></div>}

      {inspection && <div className="trust-backdrop"><section className="trust-dialog" role="dialog" aria-modal="true" aria-labelledby="trust-title" aria-describedby="trust-description">
        <div className="trust-heading"><span className="trust-icon"><Icon name="shield" size={22} /></span><div><h2 id="trust-title">Verificar este dispositivo</h2><p id="trust-description">Confirma la huella del certificado antes de enviar tu contraseña.</p></div></div>
        <dl className="certificate"><div><dt>DISPOSITIVO</dt><dd>{inspection.address}</dd></div><div><dt>SHA-256</dt><dd className="fingerprint">{inspection.fingerprint}</dd></div></dl>
        <div className="trust-actions"><span>La confianza se guarda para esta dirección.</span><button className="button secondary" type="button" onClick={() => setInspection(null)} disabled={busy}>Cancelar</button><button className="button primary" type="button" onClick={() => void acceptCertificate()} disabled={busy}>{busy ? "Conectando…" : "Confiar y conectar"}</button></div>
        {error && <p className="trust-error" role="alert">{error}</p>}
      </section></div>}
    </>
  );
}
