import { useEffect, useRef, useState } from "react";
import { call, errorMessage, nativeAvailable, onEvent, type Preferences, type SunshineStreamSettings } from "./backend";

type SunshineStatus = { state: "disconnected" | "connecting" | "connected" | "error"; message: string; source: "kvm" | "sunshine" };
type Host = { name: string; paired: boolean; apps: Array<{ id: number; title: string }> };
const initialStatus: SunshineStatus = { state: "disconnected", message: "", source: "kvm" };
const defaultStream: SunshineStreamSettings = { width: 1920, height: 1080, fps: 60, bitrateKbps: 15000, network: "auto", audioEnabled: true, volume: 100, hostAudio: true };
const resolutions = [[1280, 720], [1920, 1080], [2560, 1440], [3840, 2160]];
function newPin() {
  return String(crypto.getRandomValues(new Uint32Array(1))[0] % 10000).padStart(4, "0");
}

export default function SunshineControls({ kvmConnected, onConfigure }: { kvmConnected: boolean; onConfigure?: () => Promise<void> }) {
  const [status, setStatus] = useState(initialStatus);
  const [open, setOpen] = useState(false);
  const [ip, setIp] = useState("");
  const [port, setPort] = useState("47989");
  const [host, setHost] = useState<Host | null>(null);
  const [appId, setAppId] = useState("");
  const [pin, setPin] = useState(newPin);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [preferencesReady, setPreferencesReady] = useState(!nativeAvailable);
  const [stream, setStream] = useState(defaultStream);
  const [bitrateDraft, setBitrateDraft] = useState("15");
  const [savingStream, setSavingStream] = useState(false);
  const operation = useRef(false);
  const mounted = useRef(false);
  const closeButton = useRef<HTMLButtonElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    mounted.current = true;
    if (!nativeAvailable) return () => { mounted.current = false; };
    let revision = 0;
    let stop: (() => void) | undefined;
    let cancelled = false;
    void onEvent<SunshineStatus>("kvm://sunshine", (next) => {
      if (cancelled) return;
      revision += 1;
      setStatus(next);
    }).then((unsubscribe) => cancelled ? unsubscribe() : stop = unsubscribe).catch((e) => !cancelled && setError(errorMessage(e)));
    const currentRevision = revision;
    void call<SunshineStatus>("get_sunshine_status").then((next) => {
      if (!cancelled && revision === currentRevision) setStatus(next);
    }).catch((e) => !cancelled && setError(errorMessage(e)));
    return () => { cancelled = true; mounted.current = false; stop?.(); };
  }, []);
  useEffect(() => { if (open) closeButton.current?.focus(); }, [open]);
  useEffect(() => {
    if (!nativeAvailable) return;
    let cancelled = false;
    void call<Preferences>("get_preferences").then((saved) => {
      if (!cancelled) { setStream(saved.stream); setBitrateDraft(String(saved.stream.bitrateKbps / 1000)); }
      if (!cancelled && saved.sunshineEndpoint) {
        setIp(saved.sunshineEndpoint.ip);
        setPort(String(saved.sunshineEndpoint.port));
      }
    }).catch((e) => !cancelled && setError(errorMessage(e)))
      .finally(() => !cancelled && setPreferencesReady(true));
    return () => { cancelled = true; };
  }, []);

  async function openConfiguration() {
    try { await onConfigure?.(); setOpen(true); }
    catch (e) { setError(errorMessage(e)); }
  }

  async function updateStream(next: SunshineStreamSettings) {
    setStream(next);
    setSavingStream(true);
    try { await call("save_preferences", { patch: { stream: next } }); }
    catch (e) { setError(errorMessage(e)); }
    finally { setSavingStream(false); }
  }

  async function run(label: string, action: () => Promise<void>) {
    if (operation.current) return;
    operation.current = true;
    setBusy(label); setError("");
    try { await action(); } catch (e) { if (mounted.current) setError(errorMessage(e)); }
    finally { operation.current = false; if (mounted.current) setBusy(""); }
  }
  function applyHost(next: Host) {
    setHost(next);
    const desktop = next.apps.find((app) => /^(desktop|escritorio)$/i.test(app.title));
    setAppId(String((desktop ?? next.apps[0])?.id ?? ""));
  }
  function endpoint() {
    const number = Number(port);
    if (!Number.isInteger(number) || number < 1 || number > 65535) throw new Error("El puerto debe estar entre 1 y 65535.");
    return { ip: ip.trim(), port: number };
  }
  function close() { setOpen(false); trigger.current?.focus(); }
  const connected = status.state === "connected";
  const streamLocked = !nativeAvailable || !preferencesReady || !!busy || savingStream || connected;
  const step = connected ? "Conectado" : host?.paired ? "Listo para conectar" : host ? "Emparejamiento" : "Buscar equipo";
  const guidance = !nativeAvailable ? "Abre la aplicación de escritorio para conectar."
    : !host ? "Introduce la IP del equipo que ejecuta Sunshine."
    : !host.paired ? "Inicia el emparejamiento e introduce el PIN en Sunshine."
    : !kvmConnected ? "Conecta primero PicoKVM para habilitar el teclado y el mouse."
    : !host.apps.length ? "No hay aplicaciones disponibles. Revisa la lista de aplicaciones en Sunshine."
    : connected ? "Puedes alternar entre Sunshine y PicoKVM desde la barra superior."
    : "Selecciona una aplicación y conecta el video y el audio.";
  const shownError = error || (status.state === "error" ? status.message : "");

  return <>
    <div className="source-switch" role="group" aria-label="Fuente de video">
      <span>VIDEO</span>
      <button type="button" aria-pressed={status.source === "kvm"} disabled={!nativeAvailable || !!busy} onClick={() => void run("Cambiando la fuente…", async () => { await call("select_video_source", { source: "kvm" }); })}>PicoKVM</button>
      <button type="button" aria-pressed={status.source === "sunshine"} disabled={!nativeAvailable || !!busy || !connected} onClick={() => void run("Cambiando la fuente…", async () => { await call("select_video_source", { source: "sunshine" }); })}>Sunshine</button>
      <button ref={trigger} type="button" className="source-config" aria-haspopup="dialog" aria-expanded={open} onClick={() => void openConfiguration()}>Configurar</button>
    </div>
    {!open && shownError && <button type="button" className="source-error" onClick={() => void openConfiguration()}>Ver error Sunshine</button>}
    {open && <section className="sunshine-panel" role="dialog" aria-modal="true" aria-labelledby="sunshine-title" onKeyDown={(event) => {
      if (event.key === "Escape") { event.stopPropagation(); close(); }
      if (event.key === "Tab") {
        const controls = [...event.currentTarget.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled), select:not(:disabled)")];
        const first = controls[0]; const last = controls[controls.length - 1];
        if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
        else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
      }
    }}>
      <div className="sunshine-heading">
        <div><p className="sunshine-eyebrow">FUENTE DE VIDEO</p><h2 id="sunshine-title">Sunshine <span className="sunshine-state">{step}</span></h2></div>
        <button ref={closeButton} type="button" className="button secondary" onClick={close}>Cerrar</button>
      </div>
      <div className="sunshine-content">
        <div className="sunshine-fields">
          <label className="field"><span>IP DEL EQUIPO</span><input value={ip} placeholder="192.168.1.100" autoCapitalize="none" onChange={(e) => { setIp(e.target.value); setHost(null); }} disabled={!preferencesReady || !!busy || connected} /></label>
          <label className="field sunshine-port"><span>PUERTO HTTP</span><input type="number" min={1} max={65535} value={port} onChange={(e) => { setPort(e.target.value); setHost(null); }} disabled={!preferencesReady || !!busy || connected} /></label>
          <button type="button" className="button secondary" disabled={!nativeAvailable || !preferencesReady || !!busy || !ip.trim() || connected} onClick={() => void run("Buscando el equipo…", async () => { const target = endpoint(); applyHost(await call<Host>("inspect_sunshine", { endpoint: target })); await call("save_preferences", { patch: { sunshineEndpoint: target } }); })}>{host ? "Buscar de nuevo" : "Buscar equipo"}</button>
        </div>
        <fieldset className="sunshine-stream" disabled={streamLocked}>
          <legend>Transmisión Moonlight</legend>
          <p className="sunshine-stream-note">H.264 · Estéreo. Estos ajustes se aplican al conectar.</p>
          <div className="sunshine-stream-grid">
            <label className="field"><span>RESOLUCIÓN</span><select value={`${stream.width}x${stream.height}`} onChange={(e) => {
              const [width, height] = e.target.value.split("x").map(Number);
              void updateStream({ ...stream, width, height });
            }}>
              {!resolutions.some(([width, height]) => width === stream.width && height === stream.height) && <option value={`${stream.width}x${stream.height}`}>{stream.width} × {stream.height}</option>}
              {resolutions.map(([width, height]) => <option key={width} value={`${width}x${height}`}>{width} × {height}</option>)}
            </select></label>
            <label className="field"><span>FOTOGRAMAS / S</span><select value={stream.fps} onChange={(e) => void updateStream({ ...stream, fps: Number(e.target.value) })}>{[30, 60, 90, 120].map((fps) => <option key={fps} value={fps}>{fps} FPS</option>)}</select></label>
            <label className="field"><span>BITRATE · Mbps</span><input type="number" min={1} max={150} step={1} value={bitrateDraft} onChange={(e) => setBitrateDraft(e.target.value)} onBlur={() => {
              const value = Number(bitrateDraft);
              if (bitrateDraft.trim() && Number.isFinite(value) && value >= 1 && value <= 150) void updateStream({ ...stream, bitrateKbps: Math.round(value * 1000) });
              else { setBitrateDraft(String(stream.bitrateKbps / 1000)); setError("El bitrate debe estar entre 1 y 150 Mbps."); }
            }} onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }} /></label>
            <label className="field"><span>PERFIL DE RED</span><select value={stream.network} onChange={(e) => void updateStream({ ...stream, network: e.target.value as SunshineStreamSettings["network"] })}>
              <option value="auto">Automático</option><option value="local">Red local</option><option value="remote">Internet</option>
            </select></label>
          </div>
          <label className="sunshine-check"><input type="checkbox" checked={stream.audioEnabled} onChange={(e) => void updateStream({ ...stream, audioEnabled: e.target.checked })} />Reproducir audio en este equipo</label>
          <label className="field sunshine-volume"><span>VOLUMEN · {stream.volume} %</span><input type="range" min={0} max={100} step={5} value={stream.volume} disabled={!stream.audioEnabled} onChange={(e) => setStream({ ...stream, volume: Number(e.target.value) })} onPointerUp={(e) => void updateStream({ ...stream, volume: Number(e.currentTarget.value) })} onKeyUp={(e) => { if (["ArrowLeft", "ArrowRight", "Home", "End", "PageUp", "PageDown"].includes(e.key)) void updateStream({ ...stream, volume: Number(e.currentTarget.value) }); }} onBlur={(e) => void updateStream({ ...stream, volume: Number(e.currentTarget.value) })} /></label>
          <label className="sunshine-check"><input type="checkbox" checked={stream.hostAudio} onChange={(e) => void updateStream({ ...stream, hostAudio: e.target.checked })} />Mantener el audio en el equipo remoto</label>
        </fieldset>
        {host && !host.paired && <div className="sunshine-next sunshine-pair"><div><span className="sunshine-label">PIN DE EMPAREJAMIENTO</span><strong>{pin}</strong><button type="button" disabled={!!busy} onClick={() => setPin(newPin())}>Cambiar PIN</button></div><button type="button" className="button primary" disabled={!!busy} onClick={() => void run("Esperando que introduzcas el PIN en Sunshine…", async () => { applyHost(await call<Host>("pair_sunshine", { endpoint: endpoint(), pin })); })}>Emparejar</button></div>}
        {host?.paired && <div className="sunshine-next"><label className="field sunshine-app"><span>{host.name} · APLICACIÓN</span><select value={appId} onChange={(e) => setAppId(e.target.value)} disabled={!!busy || connected || !host.apps.length}>{host.apps.length ? host.apps.map((app) => <option key={app.id} value={app.id}>{app.title}</option>) : <option value="">Sin aplicaciones disponibles</option>}</select></label>
          {connected ? <button type="button" className="button disconnect" disabled={!!busy} onClick={() => void run("Desconectando…", async () => { await call("disconnect_sunshine"); })}>Desconectar</button> : <button type="button" className="button primary" disabled={!!busy || savingStream || !kvmConnected || !appId} onClick={() => void run("Conectando video y audio…", async () => {
            await call("connect_sunshine", { endpoint: endpoint(), appId: Number(appId), stream });
            await call("select_video_source", { source: "sunshine" });
            close();
          })}>Conectar y mostrar</button>}</div>}
        <div className={`sunshine-feedback ${shownError ? "is-error" : ""}`} role={shownError ? "alert" : "status"}>
          <span>{busy || shownError || guidance}</span>
          {shownError && !busy && <span>Revisa los datos y vuelve a intentarlo.</span>}
          {status.state === "error" && !busy && <button type="button" className="sunshine-reset" onClick={() => void run("Restableciendo Sunshine…", async () => { await call("disconnect_sunshine"); })}>Restablecer</button>}
        </div>
      </div>
    </section>}
  </>;
}
