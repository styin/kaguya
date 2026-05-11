import { createWorkletBlobUrl } from "./worklet";
import { audioRefs } from "./refs";

export interface CaptureHandle {
  stop: () => void;
}

export async function startCapture(
  onChunk: (pcm16: ArrayBuffer) => void
): Promise<CaptureHandle> {
  const ctx = new AudioContext({ sampleRate: 16000 });

  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const source = ctx.createMediaStreamSource(stream);

  const workletUrl = createWorkletBlobUrl("capture");
  await ctx.audioWorklet.addModule(workletUrl);
  URL.revokeObjectURL(workletUrl);

  const worklet = new AudioWorkletNode(ctx, "capture-processor");
  worklet.port.onmessage = (ev: MessageEvent<ArrayBuffer>) => {
    onChunk(ev.data);
  };

  // Tap point for the LeftRail mic meter. Time-domain reads in
  // meter.ts handle smoothing; keep the analyser's own smoothing low.
  const analyser = ctx.createAnalyser();
  analyser.fftSize = 256;
  analyser.smoothingTimeConstant = 0.2;
  audioRefs.mic = analyser;

  source.connect(analyser);
  analyser.connect(worklet);
  // Don't connect to destination — capture only, no feedback loop.

  return {
    stop() {
      // Only clear the shared ref if it still points at OUR analyser
      // (StrictMode double-mount safety; see playback.ts for context).
      if (audioRefs.mic === analyser) audioRefs.mic = null;
      worklet.disconnect();
      analyser.disconnect();
      source.disconnect();
      stream.getTracks().forEach((t) => t.stop());
      void ctx.close();
    },
  };
}
