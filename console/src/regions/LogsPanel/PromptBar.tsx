import { useState } from "react";
import type { IngressMessage } from "../../types";

export function PromptBar({ onSend }: { onSend: (msg: IngressMessage) => void }) {
  const [text, setText] = useState("");
  const trimmed = text.trim();

  function submit() {
    if (!trimmed) return;
    onSend({ type: "text", content: trimmed });
    setText("");
  }

  return (
    <div className="promptbar">
      <span className="prompt-prefix">›</span>
      <input
        className="prompt-input"
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            submit();
          }
        }}
        placeholder="Send a text prompt to Kaguya…"
      />
      <span className="prompt-hint">⏎ send</span>
      <button
        type="button"
        className="prompt-send"
        disabled={!trimmed}
        onClick={submit}
      >
        Send
      </button>
    </div>
  );
}
