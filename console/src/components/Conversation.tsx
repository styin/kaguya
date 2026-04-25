import { useState, useRef, useEffect } from "react";
import type { ChatEntry } from "../types";

export function Conversation({
  messages,
  onSend,
}: {
  messages: ChatEntry[];
  onSend: (text: string) => void;
}) {
  const [input, setInput] = useState("");
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    const trimmed = input.trim();
    if (!trimmed) return;
    onSend(trimmed);
    setInput("");
  }

  return (
    <div className="conversation">
      <div className="messages">
        {messages.map((msg, i) => (
          <div key={i} className={`message message-${msg.role}`}>
            <span className="message-role">
              {msg.role === "user" ? "You" : "Kaguya"}
            </span>
            <span className="message-text">{msg.content}</span>
            {msg.emotion && msg.emotion !== "neutral" && (
              <span className="message-emotion">{msg.emotion}</span>
            )}
          </div>
        ))}
        <div ref={bottomRef} />
      </div>
      <form className="input-bar" onSubmit={handleSubmit}>
        <input
          type="text"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          placeholder="Type a message..."
          autoFocus
        />
        <button type="submit">Send</button>
      </form>
    </div>
  );
}
