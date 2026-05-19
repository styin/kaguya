import { useSyncExternalStore } from "react";
import { config } from "./config";
import type {
  AudioEvent,
  AssistantTurn,
  EgressMessage,
  IngressMessage,
  Turn,
  WsEvent,
} from "./types";
import type { WsStatus } from "./ws";

// ─── State shape ─────────────────────────────────────────────────────

export interface AppState {
  events: WsEvent[];
  audioEvents: AudioEvent[];
  wsStatus: WsStatus;
  wsOpenedAt: number | null;

  selTurn: string | null;
  logsCollapsed: boolean;
  searchQuery: string;
  logSrc: "all" | "gateway" | "talker" | "llm_server";
  logLvl: "ALL" | "INFO+" | "WARN+" | "ERROR";
  micActive: boolean;
}

const initial: AppState = {
  events: [],
  audioEvents: [],
  wsStatus: "disconnected",
  wsOpenedAt: null,
  selTurn: null,
  logsCollapsed: false,
  searchQuery: "",
  logSrc: "all",
  logLvl: "ALL",
  micActive: false,
};

// ─── External store (useSyncExternalStore pattern) ───────────────────
//
// Single mutable snapshot reference behind a getter. Listeners fire on
// every mutation. Mutations always produce a new top-level reference so
// React's referential comparison detects the change. Ring arrays are
// replaced on append (no in-place mutation) for the same reason.

let state: AppState = initial;
const listeners = new Set<() => void>();

function notify() {
  for (const l of listeners) l();
}

function setState(patch: Partial<AppState>): void {
  state = { ...state, ...patch };
  notify();
}

export function getState(): AppState {
  return state;
}

export function subscribe(cb: () => void): () => void {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

export function useStore<T>(selector: (s: AppState) => T): T {
  return useSyncExternalStore(subscribe, () => selector(state));
}

// ─── Actions ─────────────────────────────────────────────────────────

function pushRing<T>(items: T[], item: T): T[] {
  return items.length >= config.eventBufferCap
    ? [...items.slice(items.length - config.eventBufferCap + 1), item]
    : [...items, item];
}

function pushEvent(ev: WsEvent): void {
  setState({ events: pushRing(state.events, ev) });
}

function pushAudioEvent(ev: AudioEvent): void {
  setState({ audioEvents: pushRing(state.audioEvents, ev) });
}

export const actions = {
  recordWsIn(msg: EgressMessage): void {
    pushEvent({ kind: "ws_in", ts: Date.now(), msg });
  },
  recordWsOut(msg: IngressMessage): void {
    pushEvent({ kind: "ws_out", ts: Date.now(), msg });
  },
  recordAudioIn(bytes: number): void {
    pushAudioEvent({ kind: "audio_in", ts: Date.now(), bytes });
  },
  recordAudioOut(bytes: number): void {
    pushAudioEvent({ kind: "audio_out", ts: Date.now(), bytes });
  },
  setWsStatus(status: WsStatus): void {
    if (status === "connected" && state.wsOpenedAt === null) {
      setState({ wsStatus: status, wsOpenedAt: Date.now() });
    } else if (status === "disconnected") {
      setState({ wsStatus: status, wsOpenedAt: null });
    } else {
      setState({ wsStatus: status });
    }
  },
  setSelTurn(turn_id: string | null): void { setState({ selTurn: turn_id }); },
  setLogsCollapsed(v: boolean): void { setState({ logsCollapsed: v }); },
  setSearchQuery(q: string): void { setState({ searchQuery: q }); },
  setLogSrc(src: AppState["logSrc"]): void { setState({ logSrc: src }); },
  setLogLvl(lvl: AppState["logLvl"]): void { setState({ logLvl: lvl }); },
  setMicActive(v: boolean): void { setState({ micActive: v }); },
};

// ─── Selectors ───────────────────────────────────────────────────────
//
// Pure functions over `events`. Memoize at the call site when needed
// (useMemo with `events` as the dep, since the array reference is
// stable across renders that don't append).

export function selectTurns(events: WsEvent[]): Turn[] {
  const turns: Turn[] = [];
  const assistantById = new Map<string, AssistantTurn>();

  for (const ev of events) {
    if (ev.kind === "ws_in") {
      const m = ev.msg;
      switch (m.event_type) {
        case "response_started": {
          const t: AssistantTurn = {
            kind: "assistant",
            turn_id: m.data.turn_id,
            started_at: ev.ts,
            complete_at: null,
            interrupted: false,
            sentences: [],
          };
          assistantById.set(m.data.turn_id, t);
          turns.push(t);
          break;
        }
        case "sentence": {
          // Append to the most recently opened, still-streaming turn.
          const open = lastOpenAssistant(turns);
          if (open) open.sentences.push({ text: m.data.text, ts: ev.ts });
          break;
        }
        case "emotion": {
          const open = lastOpenAssistant(turns);
          if (open) open.emotion = m.data.emotion;
          break;
        }
        case "response_complete": {
          const t = assistantById.get(m.data.turn_id);
          if (t) {
            t.complete_at = ev.ts;
            t.interrupted = m.data.interrupted;
          }
          break;
        }
        case "user_input": {
          turns.push({ kind: "user", ts: ev.ts, text: m.data.text, source: "voice" });
          break;
        }
      }
    } else if (ev.kind === "ws_out" && ev.msg.type === "text") {
      turns.push({ kind: "user", ts: ev.ts, text: ev.msg.content, source: "typed" });
    }
  }

  return turns;
}

function lastOpenAssistant(turns: Turn[]): AssistantTurn | null {
  for (let i = turns.length - 1; i >= 0; i--) {
    const t = turns[i];
    if (t.kind === "assistant" && t.complete_at === null) return t;
  }
  return null;
}

export function selectStreamingTurn(turns: Turn[]): AssistantTurn | null {
  for (let i = turns.length - 1; i >= 0; i--) {
    const t = turns[i];
    if (t.kind === "assistant" && t.complete_at === null) return t;
  }
  return null;
}

export function turnDurationMs(t: AssistantTurn): number | null {
  return t.complete_at === null ? null : t.complete_at - t.started_at;
}

/**
 * "Time to first sound" — `started_at` to the first `audio_in` event
 * whose `ts` falls inside the turn. This matches the conventional TTFS
 * meaning ("when does the user actually hear something") rather than
 * the sentence-event latency used previously, which fired on text-side
 * sentence boundaries before TTS had finished rendering.
 */
export function firstAudioLatencyMs(
  audioEvents: AudioEvent[],
  t: AssistantTurn,
): number | null {
  const end = t.complete_at ?? Number.POSITIVE_INFINITY;
  for (const ev of audioEvents) {
    if (ev.kind === "audio_in" && ev.ts >= t.started_at && ev.ts < end) {
      return ev.ts - t.started_at;
    }
  }
  return null;
}

export function selectWsInCount(events: WsEvent[]): number {
  return events.reduce((n, e) => n + (e.kind === "ws_in" ? 1 : 0), 0);
}

export function selectWsOutCount(events: WsEvent[]): number {
  return events.reduce((n, e) => n + (e.kind === "ws_out" ? 1 : 0), 0);
}

export function selectAudioInBytes(audioEvents: AudioEvent[]): number {
  return audioEvents.reduce((n, e) => n + (e.kind === "audio_in" ? e.bytes : 0), 0);
}

export function selectAudioOutBytes(audioEvents: AudioEvent[]): number {
  return audioEvents.reduce((n, e) => n + (e.kind === "audio_out" ? e.bytes : 0), 0);
}

/**
 * Bytes of TTS audio received during a given assistant turn — sum of
 * `audio_in` event byte counts with `ts ∈ [started_at, complete_at ?? +∞)`.
 * Derived purely from the audio event ring; no per-turn counter sits alongside it.
 */
export function audioInBytesForTurn(audioEvents: AudioEvent[], t: AssistantTurn): number {
  const end = t.complete_at ?? Number.POSITIVE_INFINITY;
  let total = 0;
  for (const ev of audioEvents) {
    if (ev.kind === "audio_in" && ev.ts >= t.started_at && ev.ts < end) {
      total += ev.bytes;
    }
  }
  return total;
}
