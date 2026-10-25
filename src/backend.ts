import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type ConnectionState = "disconnected" | "connecting" | "connected" | "error";

export interface AppStatus {
  state: ConnectionState;
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
  filter: "bilinear" | "sharp";
  sharpness: number;
}

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
