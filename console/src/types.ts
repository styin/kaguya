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

// Conversation state
export type ChatEntry = {
  role: "user" | "assistant";
  content: string;
  emotion?: string;
  // Assistant entries carry the turn_id from `response_started` so subsequent
  // sentence events fold into the same entry; a new turn (e.g. silence-
  // triggered) gets a fresh entry instead of overwriting the prior one.
  turn_id?: string;
};
