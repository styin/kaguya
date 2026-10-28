import { useCallback, useEffect, useRef } from "react";
import { createWsClient } from "./ws";
import { startCapture, type CaptureHandle } from "./audio/capture";
import type { EgressMessage, IngressMessage } from "./types";
import { actions, useStore } from "./store";
import { TurnsPanel } from "./regions/TurnsPanel/TurnsPanel";
import { Inspector } from "./regions/Inspector/Inspector";
import { LeftRail } from "./regions/LeftRail/LeftRail";
import { TopBar } from "./regions/TopBar/TopBar";
import { LogsPanel } from "./regions/LogsPanel/LogsPanel";
import "./App.css";

// Conversation state derives from the semantic event log. Microphone audio
// bypasses that log and streams directly to Gateway.

export default function App() {
  const micActive = useStore((s) => s.micActive);
  const logsCollapsed = useStore((s) => s.logsCollapsed);

  const wsRef = useRef<ReturnType<typeof createWsClient> | null>(null);
  const captureRef = useRef<CaptureHandle | null>(null);
  const handleMessage = useCallback((msg: EgressMessage) => {
    actions.recordWsIn(msg);
  }, []);

  // Shared send channel for conversational ingress. Process/app control
  // routes use POST /api/... directly and do not flow through here.
  const sendIngress = useCallback((msg: IngressMessage) => {
    actions.recordWsOut(msg);
    wsRef.current?.send(msg);
  }, []);

  const reconnect = useCallback(() => {
    wsRef.current?.reconnect();
  }, []);

  useEffect(() => {
    const client = createWsClient({
      onStatus: actions.setWsStatus,
      onMessage: handleMessage,
    });
    wsRef.current = client;
    return () => client.close();
  }, [handleMessage]);

  useEffect(() => {
    if (!micActive) return;
    let cancelled = false;
    let handle: CaptureHandle | null = null;
    startCapture((pcm16) => {
      wsRef.current?.sendBinary(pcm16);
    })
      .then((h) => {
        if (cancelled) { h.stop(); return; }
        handle = h;
        captureRef.current = h;
      })
      .catch(() => actions.setMicActive(false));
    return () => {
      cancelled = true;
      handle?.stop();
      captureRef.current = null;
    };
  }, [micActive]);

  return (
    <div className={"app" + (logsCollapsed ? " logs-collapsed" : "")}>
      <header className="region-topbar">
        <TopBar onReconnect={reconnect} />
      </header>
      <aside className="region-leftrail">
        <LeftRail />
      </aside>
      <main className="region-turns">
        <TurnsPanel />
      </main>
      <aside className="region-inspector">
        <Inspector />
      </aside>
      <footer className="region-logs">
        <LogsPanel onSend={sendIngress} />
      </footer>
    </div>
  );
}
