export const config = {
  gatewayWsUrl: `ws://${window.location.host}/ws`,
  reconnectDelays: [0, 300, 1200, 2700, 4800, 7000],
  reconnectJitterMs: 1000,
} as const;
