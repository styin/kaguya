export const config = {
  gatewayWsUrl: `ws://${window.location.host}/ws`,
  reconnectDelays: [0, 300, 1200, 2700, 4800, 7000],
  reconnectJitterMs: 1000,

  // Cap on the event ring buffer — bounded memory for long sessions.
  // Turn list and counters render off this buffer; older events fall off
  // the back. Sized for ~30 min of active conversation at typical event
  // rates (sentences + emotion + lifecycle per turn).
  eventBufferCap: 2000,

  // Cap on the log buffer. Terminal scrollback feel is the priority,
  // so this is sized generously. Older entries fall off the back.
  logBufferCap: 5000,
} as const;
