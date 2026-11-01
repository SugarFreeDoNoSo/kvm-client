import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type ConnectionState = "disconnected" | "connecting" | "connected" | "error";
export type ConnectionPhase = "idle" | "tls" | "auth" | "signaling" | "webrtc" | "ready";
export type VideoState = "waiting" | "playing" | "no_signal" | "stalled";

export interface AppStatus {
  state: ConnectionState;
  phase: ConnectionPhase;
  videoState: VideoState;
  videoPaused: boolean;
  inputEnabled: boolean;
  message: string;
  address: string;
  capture: boolean;
  captureRequested: boolean;
  inputBackend: string;
  decoder: string;
}

export interface Metrics {
  inputHz: number;
  targetHz: number;
  receivedFps: number | null;
  decodedFps: number | null;
  presentedFps: number | null;
  inputWidth: number;
  inputHeight: number;
  outputWidth: number;
  outputHeight: number;
  filterProcessingMs: number | null;
  sinkDroppedFrames: number | null;
  rttMs: number | null;
  packetsLost: number | null;
  queueDepth: number;
  congested: boolean;
}

export interface DeviceInspection {
  address: string;
  fingerprint: string;
  trusted: boolean;
  requiresTrust: boolean;
}

export interface InputSettings {
  hz: number;
  sensitivity: number;
}

export interface VideoSettings {
  filter: "original" | "desktop" | "gaming" | "nearest";
  sharpness: number;
  nativeSize: boolean;
  scaleFactor: number;
  antiRinging: number;
  edgeThreshold: number;
  downscaleAntiAliasing: boolean;
}

export interface SunshineStreamSettings {
  width: number;
  height: number;
  fps: number;
  bitrateKbps: number;
  network: "auto" | "local" | "remote";
  audioEnabled: boolean;
  volume: number;
  hostAudio: boolean;
}

export interface Preferences {
  address: string;
  recentAddresses: string[];
  sunshineEndpoint: { ip: string; port: number } | null;
  video: VideoSettings;
  input: InputSettings;
  dashboardExpanded: boolean;
  rememberPassword: boolean;
  stream: SunshineStreamSettings;
}

export type PreferencesPatch = Partial<Omit<Preferences, "recentAddresses">>;

export const nativeAvailable = isTauri();

export function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!nativeAvailable) {
    return Promise.reject(new Error("Abre la aplicación de escritorio para conectarte al KVM."));
  }
  return invoke<T>(command, args);
}

export function onEvent<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  return listen<T>(event, (message) => handler(message.payload));
}

export function errorMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return "No se pudo completar la operación.";
}
