import { useEffect, useRef, useState, type FormEvent } from "react";
import { call, errorMessage, nativeAvailable, type DeviceInspection } from "../backend";
import { MobileSession } from "./session";
import "./mobile.css";


export default function MobileApp() {
  const [panel, setPanel] = useState(true);
  const [keyboard, setKeyboard] = useState(false);
  const [address, setAddress] = useState("https://192.168.100.8/");
  const [password, setPassword] = useState("");
  const [inspection, setInspection] = useState<DeviceInspection | null>(null);
  const [busy, setBusy] = useState(false);
  const [ready, setReady] = useState(false);
  const [message, setMessage] = useState("Listo para conectar");
  const [error, setError] = useState("");
  const screen = useRef<HTMLElement>(null);
  const panelButton = useRef<HTMLButtonElement>(null);
  const addressInput = useRef<HTMLInputElement>(null);
  const session = useRef<MobileSession | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    const release = () => { session.current?.release(); };
    const visibility = () => { if (document.hidden) { release(); setKeyboard(false); } };
    window.addEventListener("blur", release);
    document.addEventListener("visibilitychange", visibility);
    return () => {
      mounted.current = false;
      session.current?.close(); session.current = null;
      window.removeEventListener("blur", release);
      document.removeEventListener("visibilitychange", visibility);
    };
  }, []);

  useEffect(() => {
    if (panel) addressInput.current?.focus();
  }, [panel]);

  useEffect(() => {
    const element = screen.current;
    if (!element || !ready) return;
    const update = () => session.current?.viewport(element.getBoundingClientRect(), !panel && !error);
    const observer = new ResizeObserver(update);
    observer.observe(element);
    window.addEventListener("resize", update);
    window.visualViewport?.addEventListener("resize", update);
    update();
    return () => {
      observer.disconnect(); window.removeEventListener("resize", update);
      window.visualViewport?.removeEventListener("resize", update);
    };
  }, [ready, panel, error, keyboard]);

  function disconnect() {
    session.current?.close(); session.current = null;

    setReady(false); setKeyboard(false); setMessage("Desconectado");
  }

  async function connect(trustedAddress?: string) {
    if (!screen.current) return;
    setBusy(true); setError("");
    try {
      const target = trustedAddress ?? address;
      if (!trustedAddress) {
        const found = await call<DeviceInspection>("mobile_inspect", { address: target });
        if (!mounted.current) return;
        if (!found.trusted) { setInspection(found); return; }
      }
      session.current?.close();
      setMessage("Conectando…"); setReady(false);
      const next = new MobileSession((status, connected) => {
        if (!mounted.current) return;
        setMessage(status); setReady(connected);
        if (connected) { setPanel(false); panelButton.current?.focus(); }
      }, (reason) => {
        if (!mounted.current) return;
        setError(errorMessage(reason)); setReady(false); setKeyboard(false); setMessage("Conexión interrumpida"); setPanel(true);
      }, setError, () => setKeyboard(false));
      session.current = next;
      await next.connect(target, password);
      if (mounted.current) setPassword("");
    } catch (reason) {
      session.current?.close(); session.current = null;
      if (mounted.current) { setError(errorMessage(reason)); setMessage("No se pudo conectar"); }
    } finally { if (mounted.current) setBusy(false); }
  }

  async function acceptTrust() {
    if (!inspection) return;
    setBusy(true); setError("");
    try {
      await call("mobile_trust", { address: inspection.address, fingerprint: inspection.fingerprint });
      setInspection(null);
      await connect(inspection.address);
    } catch (reason) { setError(errorMessage(reason)); }
    finally { setBusy(false); }
  }

  function togglePanel() {
    session.current?.release();
    setKeyboard(false);
    setPanel((open) => !open);
  }

  function toggleKeyboard() {
    if (!ready) return;
    const open = !keyboard;
    setKeyboard(open);
    if (open) {
      session.current?.keyboard(true);
    } else session.current?.keyboard(false);
  }

  function submit(event: FormEvent) { event.preventDefault(); void connect(); }

  return <div className={`mobile-app ${keyboard ? "keyboard-open" : ""}`}>
    <header className="mobile-toolbar">
      <button ref={panelButton} type="button" onClick={togglePanel} aria-expanded={panel} aria-controls="mobile-panel" aria-label={panel ? "Cerrar panel lateral" : "Abrir panel lateral"}>☰ <span>Panel</span></button>
      <span className="mobile-title">KVM Client</span>
      <button type="button" onClick={toggleKeyboard} disabled={!ready || panel} aria-expanded={keyboard} aria-controls="mobile-keyboard">⌨ <span>{keyboard ? "Ocultar" : "Teclado"}</span></button>
    </header>
    <main ref={screen} className="mobile-screen" aria-label="Pantalla remota: desliza para mover, doble toque para clic">
      {!ready && <p className="mobile-empty">Tu pantalla. Tu control.<small>Conecta tu KVM desde el panel.</small></p>}
    </main>
    <div id="mobile-keyboard" className="mobile-keyboard" data-open={keyboard}>
      <div className="mobile-special-keys" hidden={!keyboard}>
        {[["Esc", 41], ["Tab", 43], ["↑", 82], ["↓", 81], ["←", 80], ["→", 79], ["⌫", 42], ["↵", 40]].map(([label, key]) => <button type="button" key={key} onPointerDown={(event) => event.preventDefault()} onClick={() => session.current?.key(Number(key))}>{label}</button>)}
      </div>
    </div>
    <footer className="mobile-status" role="status">{message}</footer>
    {error && <div className="mobile-error" role="alert"><span>{error}</span><button type="button" onClick={() => setError("")} aria-label="Cerrar error">×</button></div>}
    {panel && <>
      <button className="mobile-panel-backdrop" aria-label="Cerrar panel lateral" onClick={togglePanel} />
      <aside id="mobile-panel" className="mobile-panel" aria-label="Conexión KVM">
        <div className="mobile-panel-heading"><h1>Conexión KVM</h1><button type="button" onClick={togglePanel} aria-label="Cerrar panel lateral">×</button></div>
        <form onSubmit={submit}>
          <label>Dirección HTTPS<input ref={addressInput} type="url" required value={address} onChange={(event) => setAddress(event.target.value)} autoCapitalize="off" autoCorrect="off" disabled={busy || !!inspection} /></label>
          <label>Contraseña<input type="password" value={password} onChange={(event) => setPassword(event.target.value)} autoComplete="off" disabled={busy || !!inspection} /></label>
          <button className="mobile-primary" disabled={busy || !nativeAvailable || !!inspection}>{busy ? "Conectando…" : "Conectar"}</button>
          <button type="button" disabled={!session.current || busy} onClick={disconnect}>Desconectar</button>
        </form>
        <p>Desliza un dedo para mover el puntero. Un toque no hace clic; dos toques rápidos hacen un clic izquierdo.</p>
        <p>Abre el teclado con el botón ⌨. El equipo remoto debe usar distribución de teclado US.</p>
        {!nativeAvailable && <p>Vista previa. La conexión requiere la aplicación Android instalada.</p>}
      </aside>
    </>}
    {inspection && <div className="mobile-trust-backdrop"><section className="mobile-trust" role="dialog" aria-modal="true" aria-labelledby="mobile-trust-title">
      <h2 id="mobile-trust-title">Verificar dispositivo</h2><p>Compara esta huella SHA-256 con la del certificado del KVM antes de enviar tu contraseña.</p>
      <p>{inspection.address}</p><code>{inspection.fingerprint}</code>
      <button type="button" disabled={busy} onClick={() => setInspection(null)}>Cancelar</button>
      <button type="button" disabled={busy} onClick={() => void acceptTrust()}>Confiar y conectar</button>
    </section></div>}
  </div>;
}
