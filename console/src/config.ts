export const config = {
  gatewayWsUrl: `ws://${window.location.host}/ws`,
  reconnectDelays: [0, 300, 1200, 2700, 4800, 7000],
  reconnectJitterMs: 1000,

  // Cap on semantic WebSocket events. Microphone frames bypass this ring.
  eventBufferCap: 2000,

  // Cap on the log buffer. Terminal scrollback feel is the priority,
  // so this is sized generously. Older entries fall off the back.
  logBufferCap: 5000,
} as const;
