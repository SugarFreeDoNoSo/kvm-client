import { useEffect, useRef, useState, type FormEvent } from "react";
import {
  call, errorMessage, nativeAvailable, onEvent,
  type AppStatus, type DeviceInspection, type InputSettings, type Metrics, type VideoSettings,
} from "./backend";

const defaultAddress = "https://192.168.100.8/";
const initialStatus: AppStatus = {
  state: "disconnected", message: "Listo para conectar", address: defaultAddress,
  capture: false, captureRequested: false, inputBackend: "", decoder: "",
};

function Icon({ name, size = 16 }: { name: "connect" | "expand" | "mouse" | "shield" | "close"; size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
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

export default function App() {
  const [address, setAddress] = useState(defaultAddress);
  const [password, setPassword] = useState("");
  const [status, setStatus] = useState<AppStatus>(initialStatus);
  const [metrics, setMetrics] = useState<Metrics | null>(null);
  const [input, setInput] = useState<InputSettings>({ hz: 125, sensitivity: 1 });
  const [sensitivityDraft, setSensitivityDraft] = useState("1");
  const [video, setVideo] = useState<VideoSettings>({ filter: "bilinear", sharpness: 0.35 });
  const [busy, setBusy] = useState(false);
  const [settingsBusy, setSettingsBusy] = useState(false);
  const [error, setError] = useState("");
  const [inspection, setInspection] = useState<DeviceInspection | null>(null);
  const [fullscreen, setFullscreen] = useState(false);
  const active = status.state === "connected";
  const connecting = status.state === "connecting";
  const locked = busy || connecting;
  const mounted = useRef(true);
  const statusRevision = useRef(0);

  useEffect(() => {
    mounted.current = true;
    if (!nativeAvailable) return () => { mounted.current = false; };
    const unsubscribe: Array<() => void> = [];
    let cancelled = false;
    let receivedStatus = false;
    void onEvent<AppStatus>("kvm://status", (next) => {
      receivedStatus = true;
      statusRevision.current += 1;
      if (!cancelled) {
        setStatus(next);
        if (next.state === "disconnected" || next.state === "error") setMetrics(null);
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

  async function refreshStatus() {
    const revision = statusRevision.current;
    const next = await call<AppStatus>("get_status");
    if (mounted.current && statusRevision.current === revision) setStatus(next);
  }

  async function connect(verifiedAddress = address) {
    await call("set_input_settings", { settings: input });
    await call("set_video_settings", { settings: video });
    await call("connect_device", { address: verifiedAddress, password });
    setPassword("");
    await refreshStatus();
  }

  async function onConnect(event: FormEvent) {
    event.preventDefault();
    if (!address.trim() || locked || !nativeAvailable) return;
    setError("");
    setBusy(true);
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
      setBusy(false);
    }
  }

  async function acceptCertificate() {
    if (!inspection || busy) return;
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
      setBusy(false);
    }
  }

  async function action(command: string, args?: Record<string, unknown>) {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await call(command, args);
      await refreshStatus();
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  }

  async function updateInput(next: InputSettings) {
    setSettingsBusy(true);
    setError("");
    try {
      await call("set_input_settings", { settings: next });
      setInput(next);
    } catch (reason) {
      setSensitivityDraft(String(input.sensitivity));
      setError(errorMessage(reason));
    } finally {
      setSettingsBusy(false);
    }
  }

  async function updateVideo(next: VideoSettings) {
    setSettingsBusy(true);
    setError("");
    try {
      await call("set_video_settings", { settings: next });
      setVideo(next);
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setSettingsBusy(false);
    }
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

  const statusText = !nativeAvailable ? "Vista previa · abre la aplicación de escritorio para conectar" : error || status.message || "Listo para conectar";

  return (
    <>
      <main className="console" aria-label="Consola IPKVM">
        <header className="topbar">
          <div className="brand">
            <span className="brand-icon" aria-hidden="true"><svg viewBox="0 0 24 24" fill="none"><path d="M4 5h16v11H4zM8 20h8M12 16v4" stroke="currentColor" strokeWidth="1.7" /><path d="m9 9-2 2 2 2m6-4 2 2-2 2" stroke="currentColor" strokeWidth="1.4" /></svg></span>
            <div className="brand-name">KVM<span>CLIENT</span></div>
            <span className="version">0.1</span>
          </div>
          <div className="session-state" data-state={nativeAvailable ? status.state : "disconnected"}>
            <span className={`state-dot ${connecting ? "pulse" : ""}`} />
            {!nativeAvailable ? "VISTA PREVIA" : active ? "EN LÍNEA" : connecting ? "CONECTANDO" : status.state === "error" ? "SIN CONEXIÓN" : "DESCONECTADO"}
          </div>
          <div className="topbar-actions">
            <span className="shortcut"><kbd>Ctrl</kbd><kbd>Alt</kbd><kbd>Esc</kbd><span>liberar control</span></span>
            <button className="icon-button" type="button" title={fullscreen ? "Salir de pantalla completa" : "Pantalla completa"} aria-label={fullscreen ? "Salir de pantalla completa" : "Pantalla completa"} onClick={() => void toggleFullscreen()} disabled={!nativeAvailable}><Icon name="expand" /></button>
          </div>
        </header>

        <form className="connection-row" onSubmit={(event) => void onConnect(event)}>
          <label className="field address-field"><span>DISPOSITIVO</span><div className="address-input"><span className="protocol-dot" /><input aria-label="Dirección del KVM" type="url" value={address} onChange={(event) => setAddress(event.target.value)} placeholder={defaultAddress} spellCheck={false} autoCapitalize="none" autoCorrect="off" required disabled={active || locked} /></div></label>
          <label className="field password-field"><span>CONTRASEÑA LOCAL</span><input aria-label="Contraseña local" type="password" value={password} onChange={(event) => setPassword(event.target.value)} placeholder="Si el dispositivo la requiere" autoComplete="off" disabled={active || locked} /></label>
          {active || connecting ? (
            <button type="button" className="button disconnect" onClick={() => void action("disconnect_device")} disabled={busy}><Icon name="connect" />Desconectar</button>
          ) : (
            <button type="submit" className="button primary" disabled={locked || !nativeAvailable || !address.trim()}><Icon name="connect" />{busy ? "Verificando…" : "Conectar"}</button>
          )}
          <button type="button" className={`button capture ${status.capture ? "captured" : ""}`} onClick={() => void action("set_capture", { enabled: !status.captureRequested })} disabled={!active || busy}><Icon name="mouse" />{status.capture ? "Liberar control" : status.captureRequested ? "Cancelar captura" : "Capturar control"}</button>
        </form>

        <section className="settings-row" aria-label="Ajustes de transmisión">
          <div className="setting-group mouse-group">
            <span className="group-label">MOUSE <span>RELATIVO</span></span>
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
            <span className="group-label">IMAGEN</span>
            <div className="segmented" role="group" aria-label="Filtro de reescalado">
              <button type="button" className={video.filter === "bilinear" ? "selected" : ""} aria-pressed={video.filter === "bilinear"} onClick={() => void updateVideo({ ...video, filter: "bilinear" })} disabled={!nativeAvailable || settingsBusy || busy}>Bilineal</button>
              <button type="button" className={video.filter === "sharp" ? "selected" : ""} aria-pressed={video.filter === "sharp"} onClick={() => void updateVideo({ ...video, filter: "sharp" })} disabled={!nativeAvailable || settingsBusy || busy}>Nitidez</button>
            </div>
            <span className="latency-badge"><span />Baja latencia</span>
          </div>
        </section>

        <section className="telemetry-row" aria-label="Métricas en vivo">
          <div className="metric"><span>VIDEO <small>recibido / decodificado / presentado</small></span><strong>{metric(metrics?.receivedFps)} <i>/</i> {metric(metrics?.decodedFps)} <i>/</i> {metric(metrics?.presentedFps)}<em>FPS</em></strong></div>
          <div className="metric"><span>MOUSE <small>efectivo / solicitado</small></span><strong>{metric(metrics?.inputHz)} <i>/</i> {input.hz}<em>Hz</em></strong></div>
          <div className="metric"><span>RTT</span><strong>{metric(metrics?.rttMs, 1)}<em>ms</em></strong></div>
          <div className="metric"><span>PÉRDIDAS</span><strong>{metric(metrics?.packetsLost)}</strong></div>
          <div className={`metric queue-metric ${metrics?.congested ? "warning" : ""}`}><span>COLA</span><strong>{metric(metrics?.queueDepth)}{metrics?.congested && <em>congestión</em>}</strong></div>
          <div className="engine-info"><span>{status.inputBackend || "Entrada nativa"}</span><span>{status.decoder || "Video nativo · WebRTC"}</span></div>
        </section>

        <footer className={`statusbar ${error || status.state === "error" ? "has-error" : ""}`}>
          <span className="status-message" role="status" title={statusText}><span className="status-square" />{statusText}</span>
          <span className="status-note">{status.capture ? "CONTROL CAPTURADO" : "1:1 · SIN PREDICCIÓN"}</span>
          {error && <button type="button" className="dismiss-error" onClick={() => setError("")} aria-label="Cerrar mensaje de error"><Icon name="close" size={12} /></button>}
        </footer>
      </main>

      {!nativeAvailable && <div className="browser-placeholder"><div className="placeholder-crosshair" aria-hidden="true">+</div><h1>Tu pantalla remota, directamente.</h1><p>El video y la captura del mouse se ejecutan en el motor nativo.</p><code>npm run tauri dev</code><span>Esta vista previa no abre conexiones al dispositivo.</span></div>}

      {inspection && <div className="trust-backdrop"><section className="trust-dialog" role="dialog" aria-modal="true" aria-labelledby="trust-title" aria-describedby="trust-description">
        <div className="trust-heading"><span className="trust-icon"><Icon name="shield" size={22} /></span><div><h2 id="trust-title">Verificar este dispositivo</h2><p id="trust-description">Confirma la huella del certificado antes de enviar tu contraseña.</p></div></div>
        <dl className="certificate"><div><dt>DISPOSITIVO</dt><dd>{inspection.address}</dd></div><div><dt>SHA-256</dt><dd className="fingerprint">{inspection.fingerprint}</dd></div></dl>
        <div className="trust-actions"><span>La confianza se guarda para esta dirección.</span><button className="button secondary" type="button" onClick={() => setInspection(null)} disabled={busy}>Cancelar</button><button className="button primary" type="button" onClick={() => void acceptCertificate()} disabled={busy}>{busy ? "Conectando…" : "Confiar y conectar"}</button></div>
        {error && <p className="trust-error" role="alert">{error}</p>}
      </section></div>}
    </>
  );
}
