// Module-level singleton holding live AnalyserNode references for the
// LeftRail audio meters. Audio nodes are mutable Web Audio objects; they
// don't belong in React state. capture.ts and playback.ts set these when
// their respective contexts start, and AudioStrip's RAF loop polls them
// directly — bypassing React re-renders entirely (no 60Hz state updates).

export const audioRefs: {
  mic: AnalyserNode | null;
  tts: AnalyserNode | null;
} = {
  mic: null,
  tts: null,
};
