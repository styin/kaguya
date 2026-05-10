import { createWorkletBlobUrl } from "./worklet";

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
  worklet.connect(ctx.destination);

  return {
    feed(pcm16: ArrayBuffer) {
      worklet.port.postMessage(pcm16, [pcm16]);
    },
    stop() {
      worklet.disconnect();
      ctx.close();
    },
  };
}
