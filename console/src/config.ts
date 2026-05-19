export const config = {
  gatewayWsUrl: `ws://${window.location.host}/ws`,
  reconnectDelays: [0, 300, 1200, 2700, 4800, 7000],
  reconnectJitterMs: 1000,

  // Cap on each in-memory event ring. Semantic WS frames and audio byte
  // counts use separate rings so high-frequency audio cannot evict turns.
  eventBufferCap: 2000,

  // Cap on the log buffer. Terminal scrollback feel is the priority,
  // so this is sized generously. Older entries fall off the back.
  logBufferCap: 5000,
} as const;
