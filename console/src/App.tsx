import { useCallback, useEffect, useRef } from "react";
import { createWsClient } from "./ws";
import { startCapture, type CaptureHandle } from "./audio/capture";
import { startPlayback, type PlaybackHandle } from "./audio/playback";
import type { EgressMessage, IngressMessage } from "./types";
import { actions, useStore } from "./store";
import { TurnsPanel } from "./regions/TurnsPanel/TurnsPanel";
import { Inspector } from "./regions/Inspector/Inspector";
import { LeftRail } from "./regions/LeftRail/LeftRail";
import { TopBar } from "./regions/TopBar/TopBar";
import { LogsPanel } from "./regions/LogsPanel/LogsPanel";
import "./App.css";

// WS frames and outgoing sends are recorded into the semantic event log in
// `store.ts`; audio frames use a separate byte-count ring so an open mic
// cannot evict turn lifecycle events. UI state derives from selectors over
// those logs; nothing stores a separate `messages` mirror.

export default function App() {
  const micActive = useStore((s) => s.micActive);
  const logsCollapsed = useStore((s) => s.logsCollapsed);

  const wsRef = useRef<ReturnType<typeof createWsClient> | null>(null);
  const captureRef = useRef<CaptureHandle | null>(null);
  const playbackRef = useRef<PlaybackHandle | null>(null);

  const handleAudio = useCallback((data: ArrayBuffer) => {
    actions.recordAudioIn(data.byteLength);
    playbackRef.current?.feed(data);
  }, []);

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
      onAudio: handleAudio,
    });
    wsRef.current = client;
    return () => client.close();
  }, [handleMessage, handleAudio]);

  useEffect(() => {
    // StrictMode double-mounts in dev: cancel the pending startPlayback
    // so a stale handle from mount-1 doesn't survive past mount-2's
    // cleanup. Without this, two audio graphs run concurrently and the
    // meter can latch onto the discarded one.
    let cancelled = false;
    let handle: PlaybackHandle | null = null;
    startPlayback().then((h) => {
      if (cancelled) { h.stop(); return; }
      handle = h;
      playbackRef.current = h;
    });
    return () => {
      cancelled = true;
      handle?.stop();
      playbackRef.current = null;
    };
  }, []);

  useEffect(() => {
    if (!micActive) return;
    let cancelled = false;
    let handle: CaptureHandle | null = null;
    startCapture((pcm16) => {
      actions.recordAudioOut(pcm16.byteLength);
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
