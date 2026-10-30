import { useCallback } from "react";
import { audioRefs } from "../../audio/refs";
import { useMeterBars } from "../../audio/meter";

const BAR_COUNT = 24;

export function AudioStrip({ detail }: { detail: string }) {
  const getAnalyser = useCallback(() => audioRefs.mic, []);
  const bars = useMeterBars(getAnalyser, BAR_COUNT);
  return (
    <div className="audio-strip">
      <div className="audio-arrow mic-arrow">↑</div>
      <div className="audio-strip-body">
        <div className="audio-meta">
          <span className="audio-meta-name">mic</span>
          <span className="audio-meta-detail">{detail}</span>
        </div>
        <div className="audio-bars mic">
          {bars.map((ref, i) => (
            <span key={i} ref={ref as React.Ref<HTMLSpanElement>} className="audio-bar" />
          ))}
        </div>
      </div>
    </div>
  );
}
