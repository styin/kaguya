import { useCallback, useEffect, useRef, useState } from "react";
import { Toolbar, type ProcessInfo } from "./components/Toolbar";
import { Conversation } from "./components/Conversation";
import { LogPanel, type LogEntry } from "./components/LogPanel";
import { createWsClient, type WsStatus } from "./ws";
import { startCapture, type CaptureHandle } from "./audio/capture";
import { startPlayback, type PlaybackHandle } from "./audio/playback";
import type { ChatEntry, EgressMessage } from "./types";
import "./App.css";

export default function App() {
  const [wsStatus, setWsStatus] = useState<WsStatus>("disconnected");
  const [messages, setMessages] = useState<ChatEntry[]>([]);
  const [processes, setProcesses] = useState<ProcessInfo[]>([]);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [micActive, setMicActive] = useState(false);
  const wsRef = useRef<ReturnType<typeof createWsClient> | null>(null);
  const captureRef = useRef<CaptureHandle | null>(null);
  const playbackRef = useRef<PlaybackHandle | null>(null);

  const pendingRef = useRef<string>("");
  const emotionRef = useRef<string | undefined>(undefined);

  const handleAudio = useCallback((data: ArrayBuffer) => {
    playbackRef.current?.feed(data);
  }, []);

  const handleMessage = useCallback((msg: EgressMessage) => {
    switch (msg.event_type) {
      case "response_started":
        pendingRef.current = "";
        emotionRef.current = undefined;
        break;

      case "sentence":
        pendingRef.current +=
          (pendingRef.current ? " " : "") + msg.data.text;
        setMessages((prev) => {
          const last = prev[prev.length - 1];
          if (last?.role === "assistant") {
            return [
              ...prev.slice(0, -1),
              { ...last, content: pendingRef.current, emotion: emotionRef.current },
            ];
          }
          return [...prev, { role: "assistant", content: pendingRef.current }];
        });
        break;

      case "emotion":
        emotionRef.current = msg.data.emotion;
        setMessages((prev) => {
          const last = prev[prev.length - 1];
          if (last?.role === "assistant") {
            return [
              ...prev.slice(0, -1),
              { ...last, emotion: msg.data.emotion },
            ];
          }
          return prev;
        });
        break;

      case "response_complete":
        pendingRef.current = "";
        emotionRef.current = undefined;
        break;
    }
  }, []);

  // WebSocket connection
  useEffect(() => {
    const client = createWsClient({
      onStatus: setWsStatus,
      onMessage: handleMessage,
      onAudio: handleAudio,
    });
    wsRef.current = client;
    return () => client.close();
  }, [handleMessage, handleAudio]);

  // Playback — always running, feeds from WS binary frames
  useEffect(() => {
    let handle: PlaybackHandle | null = null;
    startPlayback().then((h) => {
      handle = h;
      playbackRef.current = h;
    });
    return () => {
      handle?.stop();
      playbackRef.current = null;
    };
  }, []);

  // Mic capture — toggled by user
  useEffect(() => {
    if (!micActive) return;
    let handle: CaptureHandle | null = null;
    startCapture((pcm16) => {
      wsRef.current?.sendBinary(pcm16);
    }).then((h) => {
      handle = h;
      captureRef.current = h;
    }).catch(() => {
      setMicActive(false);
    });
    return () => {
      handle?.stop();
      captureRef.current = null;
    };
  }, [micActive]);

  // SSE for real-time logs
  useEffect(() => {
    const es = new EventSource("/api/logs/stream");
    es.onmessage = (ev) => {
      const entry: LogEntry = JSON.parse(ev.data);
      setLogs((prev) => {
        const next = [...prev, entry];
        return next.length > 10_000 ? next.slice(-10_000) : next;
      });
    };
    return () => es.close();
  }, []);

  // Poll process status
  useEffect(() => {
    let active = true;
    async function poll() {
      if (!active) return;
      try {
        const res = await fetch("/api/process/status");
        if (res.ok) setProcesses(await res.json());
      } catch { /* supervisor not ready */ }
    }
    poll();
    const interval = setInterval(poll, 1000);
    return () => { active = false; clearInterval(interval); };
  }, []);

  function handleSend(text: string) {
    setMessages((prev) => [...prev, { role: "user", content: text }]);
    wsRef.current?.send({ type: "text", content: text });
  }

  function handleReconnect() {
    wsRef.current?.reconnect();
  }

  function handleMicToggle() {
    setMicActive((prev) => !prev);
  }

  async function handleProcessAction(
    name: string,
    action: "start" | "stop" | "restart"
  ) {
    await fetch(`/api/process/${name}/${action}`, { method: "POST" });
  }

  return (
    <div className="app">
      <Toolbar
        wsStatus={wsStatus}
        onReconnect={handleReconnect}
        processes={processes}
        onProcessAction={handleProcessAction}
      />
      <div className="main-content">
        <Conversation
          messages={messages}
          onSend={handleSend}
          micActive={micActive}
          onMicToggle={handleMicToggle}
        />
      </div>
      <LogPanel logs={logs} />
    </div>
  );
}
