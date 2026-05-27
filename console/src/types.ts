// Ingress: browser → Gateway
export type TextMessage = { type: "text"; content: string };
export type ControlMessage = { type: "control"; command: "stop" | "shutdown" };
export type IngressMessage = TextMessage | ControlMessage;

// Egress: Gateway → browser
export type SentenceEvent = { event_type: "sentence"; data: { text: string } };
export type EmotionEvent = { event_type: "emotion"; data: { emotion: string } };
export type ResponseStartedEvent = {
  event_type: "response_started";
  data: { turn_id: string };
};
export type ResponseCompleteEvent = {
  event_type: "response_complete";
  data: { turn_id: string; interrupted: boolean };
};
// Echo of voice transcripts so the UI can render them as user messages.
// Typed prompts are added locally by the input form and are NOT echoed.
export type UserInputEvent = {
  event_type: "user_input";
  data: { text: string };
};

export type EgressMessage =
  | SentenceEvent
  | EmotionEvent
  | ResponseStartedEvent
  | ResponseCompleteEvent
  | UserInputEvent;

// ─── Event log envelope ──────────────────────────────────────────────
//
// Direction is named relative to the browser (where this code runs):
//   `ws_in`    / `audio_in`  — browser RECEIVES
//   `ws_out`   / `audio_out` — browser SENDS
//
// Semantic UI state derives from the WS event log via pure selectors in `store.ts`.
// Audio byte counts live in a separate ring so they cannot evict turn events.
// Audio frames carry only byte counts, never PCM payloads.

export type WsEvent =
  | { kind: "ws_in";     ts: number; msg: EgressMessage }
  | { kind: "ws_out";    ts: number; msg: IngressMessage };

export type AudioEvent =
  | { kind: "audio_in";  ts: number; bytes: number }
  | { kind: "audio_out"; ts: number; bytes: number };

// ─── Turn projection (derived) ───────────────────────────────────────
//
// Computed by `selectTurns()` over the event log; never stored directly.
// Each `response_started` opens an assistant turn; `sentence`/`emotion`
// append; `response_complete` closes it. User turns come from
// `user_input` (voice) or `ws_out` text sends (typed prompts).

export type Sentence = { text: string; ts: number };

export type AssistantTurn = {
  kind: "assistant";
  turn_id: string;
  started_at: number;
  complete_at: number | null;
  interrupted: boolean;
  sentences: Sentence[];
  emotion?: string;
};

export type UserTurn = {
  kind: "user";
  ts: number;
  text: string;
  source: "voice" | "typed";
};

export type Turn = AssistantTurn | UserTurn;

// ─── Supervisor HTTP types ───────────────────────────────────────────
//
// Mirror of `console/server/supervisor.ts` shapes. Duplicated here
// because tsconfig.json scopes `include` to `src/` only.

export type ProcessStatus = "stopped" | "starting" | "running" | "errored";

export interface ProcessInfo {
  name: string;
  label: string;
  managed: boolean;
  status: ProcessStatus;
  group?: string;
  conflicts?: string[];
  blockedBy?: string[];
  pid?: number;
  uptimeSecs?: number;
  exitCode?: number | null;
}

export interface LogEntry {
  id: number;
  timestamp: string;
  source: string;
  stream: "stdout" | "stderr";
  line: string;
}
