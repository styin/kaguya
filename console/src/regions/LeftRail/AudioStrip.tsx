import { useCallback } from "react";
import { audioRefs } from "../../audio/refs";
import { useMeterBars } from "../../audio/meter";

const BAR_COUNT = 24;

export function AudioStrip({
  kind,
  name,
  detail,
}: {
  kind: "mic" | "tts";
  name: string;
  detail: string;
}) {
  const getAnalyser = useCallback(
    () => (kind === "mic" ? audioRefs.mic : audioRefs.tts),
    [kind],
  );
  const bars = useMeterBars(getAnalyser, BAR_COUNT);

  return (
    <div className="audio-strip">
      <div className={"audio-arrow " + (kind === "mic" ? "mic-arrow" : "tts-arrow")}>
        {kind === "mic" ? "↑" : "↓"}
      </div>
      <div className="audio-strip-body">
        <div className="audio-meta">
          <span className="audio-meta-name">{name}</span>
          <span className="audio-meta-detail">{detail}</span>
        </div>
        <div className={"audio-bars " + kind}>
          {bars.map((ref, i) => (
            <span key={i} ref={ref as React.Ref<HTMLSpanElement>} className="audio-bar" />
          ))}
        </div>
      </div>
    </div>
  );
}
