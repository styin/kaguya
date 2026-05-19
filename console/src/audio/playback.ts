import { createWorkletBlobUrl } from "./worklet";
import { audioRefs } from "./refs";

export interface PlaybackHandle {
  feed: (pcm16: ArrayBuffer) => void;
  stop: () => void;
}

export async function startPlayback(): Promise<PlaybackHandle> {
  const ctx = new AudioContext({ sampleRate: 16000 });

  const workletUrl = createWorkletBlobUrl("playback");
  await ctx.audioWorklet.addModule(workletUrl);
  URL.revokeObjectURL(workletUrl);

  const worklet = new AudioWorkletNode(ctx, "playback-processor");

  // Tap point for the LeftRail TTS meter. Low `smoothingTimeConstant`
  // (or none) because the visual easing already lives in `meter.ts`
  // — asymmetric attack/release runs there, not in the analyser.
  const analyser = ctx.createAnalyser();
  analyser.fftSize = 256;
  analyser.smoothingTimeConstant = 0.2;
  audioRefs.tts = analyser;

  worklet.connect(analyser);
  analyser.connect(ctx.destination);

  // Chrome's autoplay policy starts new AudioContexts in `suspended`
  // until a user gesture. Try eagerly here — in case we're already
  // past a gesture — and also wire a one-shot listener as a fallback
  // for contexts created before any interaction.
  void ctx.resume();
  const detach = attachGestureResume(ctx);

  return {
    feed(pcm16: ArrayBuffer) {
      worklet.port.postMessage(pcm16, [pcm16]);
    },
    stop() {
      detach();
      // Only clear the shared ref if it still points at OUR analyser.
      // React StrictMode double-mounts the playback effect in dev: a
      // discarded mount's stop() must not wipe the live mount's
      // analyser, otherwise the meter goes idle while audio plays.
      if (audioRefs.tts === analyser) audioRefs.tts = null;
      worklet.disconnect();
      analyser.disconnect();
      void ctx.close();
    },
  };
}

function attachGestureResume(ctx: AudioContext): () => void {
  function resume() {
    if (ctx.state === "suspended") void ctx.resume();
  }
  document.addEventListener("pointerdown", resume, { once: true });
  document.addEventListener("keydown", resume, { once: true });
  return () => {
    document.removeEventListener("pointerdown", resume);
    document.removeEventListener("keydown", resume);
  };
}
