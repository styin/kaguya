import { createWorkletBlobUrl } from "./worklet";

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

  source.connect(worklet);
  // Don't connect to destination — capture only, no feedback loop

  return {
    stop() {
      worklet.disconnect();
      source.disconnect();
      stream.getTracks().forEach((t) => t.stop());
      ctx.close();
    },
  };
}
