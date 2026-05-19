// AudioWorkletProcessor for mic capture and TTS playback.
// Runs in the audio thread — no DOM access, no imports.
// Registered by capture.ts and playback.ts via addModule().

const CAPTURE_PROCESSOR = `
class CaptureProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this._buffer = new Float32Array(0);
  }

  process(inputs) {
    const input = inputs[0]?.[0];
    if (!input) return true;

    // Accumulate samples
    const merged = new Float32Array(this._buffer.length + input.length);
    merged.set(this._buffer);
    merged.set(input, this._buffer.length);
    this._buffer = merged;

    // Flush 20ms chunks (320 samples at 16kHz)
    while (this._buffer.length >= 320) {
      const chunk = this._buffer.slice(0, 320);
      this._buffer = this._buffer.slice(320);

      // float32 → int16
      const pcm = new Int16Array(320);
      for (let i = 0; i < 320; i++) {
        const s = Math.max(-1, Math.min(1, chunk[i]));
        pcm[i] = s < 0 ? s * 0x8000 : s * 0x7FFF;
      }
      this.port.postMessage(pcm.buffer, [pcm.buffer]);
    }
    return true;
  }
}
registerProcessor("capture-processor", CaptureProcessor);
`;

const PLAYBACK_PROCESSOR = `
class PlaybackProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this._ring = new Float32Array(0);
    this.port.onmessage = (e) => {
      const pcm16 = new Int16Array(e.data);
      const float32 = new Float32Array(pcm16.length);
      for (let i = 0; i < pcm16.length; i++) {
        float32[i] = pcm16[i] / (pcm16[i] < 0 ? 0x8000 : 0x7FFF);
      }
      const merged = new Float32Array(this._ring.length + float32.length);
      merged.set(this._ring);
      merged.set(float32, this._ring.length);
      this._ring = merged;
    };
  }

  process(_inputs, outputs) {
    const output = outputs[0]?.[0];
    if (!output) return true;

    const needed = output.length;
    if (this._ring.length >= needed) {
      output.set(this._ring.slice(0, needed));
      this._ring = this._ring.slice(needed);
    } else {
      // Underrun — output silence for missing samples
      output.set(this._ring);
      this._ring = new Float32Array(0);
    }
    return true;
  }
}
registerProcessor("playback-processor", PlaybackProcessor);
`;

export function createWorkletBlobUrl(type: "capture" | "playback"): string {
  const code = type === "capture" ? CAPTURE_PROCESSOR : PLAYBACK_PROCESSOR;
  const blob = new Blob([code], { type: "application/javascript" });
  return URL.createObjectURL(blob);
}
