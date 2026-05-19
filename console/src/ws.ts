import { config } from "./config";
import type { IngressMessage, EgressMessage } from "./types";

export type WsStatus = "connecting" | "connected" | "disconnected";

export type WsCallbacks = {
  onStatus: (status: WsStatus) => void;
  onMessage: (msg: EgressMessage) => void;
  onAudio: (data: ArrayBuffer) => void;
};

export function createWsClient(callbacks: WsCallbacks) {
  let ws: WebSocket | null = null;
  let attempt = 0;
  let closed = false;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

  function cancelPending() {
    if (reconnectTimer !== null) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
  }

  function connect() {
    if (closed) return;
    cancelPending();
    // Close any existing connection before opening a new one
    if (ws) {
      ws.onclose = null;
      ws.close();
      ws = null;
    }
    callbacks.onStatus("connecting");
    ws = new WebSocket(config.gatewayWsUrl);
    ws.binaryType = "arraybuffer";

    ws.onopen = () => {
      attempt = 0;
      callbacks.onStatus("connected");
    };

    ws.onmessage = (ev: MessageEvent) => {
      if (ev.data instanceof ArrayBuffer) {
        callbacks.onAudio(ev.data);
        return;
      }
      try {
        const msg = JSON.parse(ev.data as string) as EgressMessage;
        callbacks.onMessage(msg);
      } catch {
        // ignore malformed JSON
      }
    };

    ws.onclose = () => {
      ws = null;
      callbacks.onStatus("disconnected");
      scheduleReconnect();
    };

    ws.onerror = () => {
      ws?.close();
    };
  }

  function scheduleReconnect() {
    if (closed) return;
    cancelPending();
    const delays = config.reconnectDelays;
    if (attempt >= delays.length) return;

    let delay = delays[attempt];
    if (attempt > 0) {
      delay += Math.random() * config.reconnectJitterMs;
    }
    attempt++;
    reconnectTimer = setTimeout(connect, delay);
  }

  function send(msg: IngressMessage) {
    if (ws?.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify(msg));
    }
  }

  function sendBinary(data: ArrayBuffer) {
    if (ws?.readyState === WebSocket.OPEN) {
      ws.send(data);
    }
  }

  function reconnect() {
    attempt = 0;
    connect();
  }

  function close() {
    closed = true;
    cancelPending();
    ws?.close();
  }

  connect();

  return { send, sendBinary, reconnect, close };
}
