import { useEffect, useState } from "react";
import type { AssistantTurn, AudioEvent, Turn } from "../../types";
import { firstAudioLatencyMs, turnDurationMs } from "../../store";

export function turnKey(t: Turn): string {
  return t.kind === "assistant" ? `assistant:${t.turn_id}` : `user:${t.ts}`;
}

export function turnBody(t: Turn): string {
  return t.kind === "assistant" ? t.sentences.map((s) => s.text).join(" ") : t.text;
}

export function TurnRow({
  turn,
  selectedKey,
  onSelect,
  audioEvents,
  audioInBytes,
}: {
  turn: Turn;
  selectedKey: string | null;
  onSelect: (key: string | null) => void;
  audioEvents: AudioEvent[];
  audioInBytes: number;
}) {
  const k = turnKey(turn);
  const selected = selectedKey === k;
  const dimmed = selectedKey !== null && !selected;
  const isUser = turn.kind === "user";
  const streaming = turn.kind === "assistant" && turn.complete_at === null;
  const interrupted = turn.kind === "assistant" && turn.interrupted;
  const ts = isUser ? turn.ts : (turn as AssistantTurn).started_at;

  const cls =
    "turn-row" +
    (isUser ? " user" : "") +
    (selected ? " selected" : "") +
    (streaming ? " live" : "") +
    (dimmed ? " dim" : "");

  return (
    <div className={cls} onClick={() => onSelect(selected ? null : k)}>
      <div className="stripes">
        {streaming ? (
          <span className="stripe live" />
        ) : interrupted ? (
          <span className="stripe interrupted" />
        ) : (
          <span className="stripe quiet" />
        )}
      </div>
      <div className="turn-time">
        <div className="abs">{formatAbs(ts)}</div>
        <div className="rel"><RelativeTime ts={ts} /></div>
      </div>
      <div className="turn-body">
        <div className="turn-who">
          <span className="who-name">{isUser ? "User" : "Kaguya"}</span>
          {!isUser && (
            <span className="turn-id mono">{(turn as AssistantTurn).turn_id.slice(0, 8)}</span>
          )}
          <span className="turn-tags">
            {streaming && <span className="tag live">live</span>}
            {interrupted && <span className="tag interrupted">interrupted</span>}
          </span>
        </div>
        <div className={"turn-text" + (isUser ? " user" : "") + (streaming ? " streaming" : "")}>
          {turnBody(turn) || (streaming ? "…" : "")}
          {streaming && <span className="ellipsis-fade">…</span>}
        </div>
        {!isUser && (
          <AssistantMeta
            turn={turn as AssistantTurn}
            audioInBytes={audioInBytes}
            audioEvents={audioEvents}
          />
        )}
        {isUser && (
          <div className="turn-meta-mini"><span>{turn.text.length} chars · {(turn as Extract<Turn, { kind: "user" }>).source}</span></div>
        )}
      </div>
      {/*
        Design has an audio-mini waveform in this slot. Skipped: we keep
        only byte counts in the audio event ring (`audio_in` events have `bytes`
        but no PCM payload), so a real waveform would require either
        keeping audio in memory or new server-side metadata. The audio
        KB in the meta-mini line above already gives a magnitude.
      */}
    </div>
  );
}

function AssistantMeta({
  turn,
  audioInBytes,
  audioEvents,
}: {
  turn: AssistantTurn;
  audioInBytes: number;
  audioEvents: AudioEvent[];
}) {
  const ttfs = firstAudioLatencyMs(audioEvents, turn);
  const dur = turnDurationMs(turn);
  return (
    <div className="turn-meta-mini">
      <span>TTFS <b>{ttfs === null ? "—" : `${ttfs}ms`}</b></span>
      <span>dur <b>{dur === null ? "—" : `${(dur / 1000).toFixed(2)}s`}</b></span>
      <span>sent <b>{turn.sentences.length}{turn.complete_at === null ? "…" : ""}</b></span>
      <span>audio <b>{(audioInBytes / 1024).toFixed(1)} KB</b></span>
      {turn.emotion && <span>emo <b>{turn.emotion}</b></span>}
    </div>
  );
}

/** Self-ticking relative-time label (e.g. "2m 14s ago"). */
function RelativeTime({ ts }: { ts: number }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);
  const secs = Math.floor((now - ts) / 1000);
  if (secs < 60) return <>−{secs}s</>;
  return <>−{Math.floor(secs / 60)}m {secs % 60}s</>;
}

function formatAbs(ms: number): string {
  const d = new Date(ms);
  const h = String(d.getHours()).padStart(2, "0");
  const m = String(d.getMinutes()).padStart(2, "0");
  const s = String(d.getSeconds()).padStart(2, "0");
  return `${h}:${m}:${s}`;
}

