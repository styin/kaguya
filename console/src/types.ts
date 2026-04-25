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

export type EgressMessage =
  | SentenceEvent
  | EmotionEvent
  | ResponseStartedEvent
  | ResponseCompleteEvent;

// Conversation state
export type ChatEntry = {
  role: "user" | "assistant";
  content: string;
  emotion?: string;
};
