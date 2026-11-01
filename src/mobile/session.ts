import { addPluginListener, invoke, type PluginListener } from "@tauri-apps/api/core";
import { call, onEvent } from "../backend";

interface SignalEvent { session: string; kind: string; data: string }

/** Only low-frequency signaling crosses JS. Video, touch and IME stay native. */
export class MobileSession {
  private id = crypto.randomUUID();
  private stopListening?: () => void;
  private nativeListener?: PluginListener;
  private closed = false;
  private started = false;
  private signals = Promise.resolve();
  private timeout?: ReturnType<typeof setTimeout>;

  constructor(private state: (message: string, ready: boolean) => void, private failure: (error: unknown) => void, private warning: (message: string) => void) {}

  private native(action: string, args: Record<string, unknown> = {}) {
    return invoke("plugin:kvm-mobile|execute", { action, session: this.id, ...args });
  }

  async connect(address: string, password: string) {
    this.nativeListener = await addPluginListener<SignalEvent>("kvm-mobile", "session", (event) => {
      if (event.session !== this.id || this.closed) return;
      this.signals = this.signals.then(async () => {
        if (this.closed) return;
        if (event.kind === "ready") {
          clearTimeout(this.timeout);
          this.state("Conectado · desliza para mover; doble toque para clic", true);
        } else if (event.kind === "error") throw new Error(event.data);
        else if (event.kind === "warning") this.warning(event.data);
        else if (event.kind === "offer") {
          const bytes = new TextEncoder().encode(JSON.stringify({ type: "offer", sdp: event.data }));
          await this.signal({ type: "offer", data: { sd: btoa(Array.from(bytes, (byte) => String.fromCharCode(byte)).join("")) } });
        } else if (event.kind === "ice") await this.signal({ type: "new-ice-candidate", data: JSON.parse(event.data) });
      }).catch((error) => this.fail(error));
    });
    if (this.closed) { await this.nativeListener.unregister(); return; }
    await this.native("start");
    this.stopListening = await onEvent<SignalEvent>("kvm://mobile-signal", (event) => {
      if (event.session !== this.id || this.closed) return;
      this.signals = this.signals.then(async () => {
        if (this.closed) return;
        if (event.kind === "closed") throw new Error(event.data || "Conexión cerrada");
        if (event.data === "pong") return;
        const message = JSON.parse(event.data);
        if (message.type === "device-metadata" && !this.started) {
          this.started = true;
          await this.native("offer");
        } else if (message.type === "answer") {
          const answer = new TextDecoder().decode(Uint8Array.from(atob(message.data), (char) => char.charCodeAt(0)));
          await this.native("answer", { data: answer });
        } else if (message.type === "new-ice-candidate") await this.native("ice", { data: JSON.stringify(message.data) });
      }).catch((error) => this.fail(error));
    });
    if (this.closed) { this.stopListening(); await this.native("close").catch(() => {}); return; }
    this.timeout = setTimeout(() => this.fail(new Error("Tiempo agotado negociando WebRTC")), 30000);
    await call("mobile_connect", { address, password, session: this.id });
    if (this.closed) await call("mobile_disconnect", { session: this.id });
  }

  viewport(rect: DOMRect, visible: boolean) {
    if (this.closed) return;
    void this.native("viewport", { x: rect.x, y: rect.y, width: rect.width, height: rect.height, visible }).catch((error) => this.fail(error));
  }
  keyboard(visible: boolean) { if (!this.closed) void this.native("keyboard", { visible }).catch((error) => this.fail(error)); }
  key(key: number) { if (!this.closed) void this.native("key", { data: JSON.stringify({ key, modifier: 0 }) }).catch((error) => this.fail(error)); }
  release() { if (!this.closed) void this.native("release").catch(() => {}); }
  private signal(message: unknown) { return call("mobile_signal", { session: this.id, message: JSON.stringify(message) }); }
  private fail(error: unknown) { if (!this.closed) { this.close(); this.failure(error); } }
  close() {
    if (this.closed) return;
    this.closed = true; clearTimeout(this.timeout); this.stopListening?.();
    void this.nativeListener?.unregister().catch(() => {});
    void this.native("close").catch(() => {});
    void call("mobile_disconnect", { session: this.id }).catch(() => {});
  }
}
