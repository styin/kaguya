import type { AssistantTurn } from "../../types";
import { turnBody } from "../TurnsPanel/TurnRow";

/**
 * Live monitor: streaming-turn card. Renders the in-flight or most-recent
 * assistant turn. While streaming, the body text pulses and a skel-row
 * appears below to indicate more sentences are inbound.
 */
export function LiveMonitor({
  streaming,
  latest,
}: {
  streaming: AssistantTurn | null;
  latest: AssistantTurn | null;
}) {
  const turn = streaming ?? latest;
  const isStreaming = streaming !== null;

  return (
    <div className="inspector">
      <div className="inspector-card">
        <div className="live-card-header">
          <span className={"live-pin " + (isStreaming ? "streaming" : "latest")}>
            {isStreaming ? "streaming" : "latest"}
          </span>
        </div>
        {turn ? (
          <>
            <div className={"live-body" + (isStreaming ? " streaming" : "")}>
              {turnBody(turn) || (isStreaming ? "…" : "")}
            </div>
            {isStreaming && <div className="skel-row" style={{ width: "60%" }} />}
          </>
        ) : (
          <div className="inspector-empty">no turns yet</div>
        )}
      </div>
    </div>
  );
}
