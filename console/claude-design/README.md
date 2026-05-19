# Handoff: Kaguya Developer Console UI Redesign

## Overview

A redesign of Kaguya's developer console — the local web app that supervises gateway / talker / llm_server processes, monitors live audio I/O, and lets a developer inspect every turn of a conversation. The new console replaces the bare bones React front-end at `styin/kaguya@sty_dev-ui/console` with a calm, paper-and-pastel-blue UI organized around four persistent regions: a top bar, a left rail (audio + processes), a center turns panel with a transcript timeline, and a right-side inspector. A collapsible logs panel pinned to the bottom doubles as a text-prompt bar.

## About the Design Files

The files in this bundle are **design references created in HTML** — they are prototypes showing the intended look and behavior, **not production code to copy directly**. Your job is to **recreate these designs inside the existing Kaguya console codebase** (Vite + React + TypeScript at `console/`), reusing its WebSocket plumbing, types, and supervisor plugin. The existing components (`App.tsx`, `Toolbar.tsx`, `Conversation.tsx`, `LogPanel.tsx`, `ws.ts`, `types.ts`) should be refactored / replaced to match the new structure rather than the HTML being shipped wholesale.

The HTML uses React 18 + Babel inline via UMD scripts purely for prototype convenience. In the real codebase you'll have proper TSX modules, your bundler, your test setup, and your typed message contracts from `proto/kaguya/v1/kaguya.proto`.

## Fidelity

**High-fidelity.** Final colors, typography scale, spacing, border-radii, shadows, hover states, micro-animations (skeleton shimmers, live pulse, meter bars) are committed. Recreate pixel-for-pixel using the codebase's existing tooling (CSS Modules, Tailwind, vanilla CSS — whatever's already there; the existing console uses plain `App.css`, so plain CSS is the path of least resistance).

The transcript timeline lane data, audio meter bars, and skeleton placeholders are mocked in the prototype — wire them to the real WS message stream (`TranscriptDeltaMessage`, `AudioOutputMessage`, `TurnLifecycleMessage`, etc.) from `types.ts` / `kaguya.proto`.

## Files in this Bundle

- `Console.html` — the full hi-fi console design. Open this first. Contains every screen state in one file.
- `Audio Devices.html` — five alternative module shapes for the audio I/O cards. **The chosen direction is Option 5 ("Envelopes — oscilloscope strips")** and it's already what's in `Console.html`. The other four options are kept here as reference / future revisits.

## Layout — Top Level

The whole app is a CSS Grid:

```
grid-template-rows:    44px 1fr var(--logs-h, 240px);
grid-template-columns: 248px 1fr 360px;
```

- **Row 1 (44px):** Top bar, full width
- **Row 2 (1fr):** Left rail (248px) · Center turns panel (1fr) · Right inspector (360px)
- **Row 3 (240px, collapsible to `min-content`):** Logs panel, full width, with a pinned prompt bar on top

When logs are collapsed (`.app.logs-collapsed`), Row 3 shrinks to fit just the prompt-bar + log-toolbar; the prompt-bar stays visible so text prompting always works.

## Screens / Views

### 1. Top Bar (`44px` tall, full-width)

- **Left:** Kaguya wordmark `Kaguya` (Newsreader 500 italic, 16px, `var(--ink)`) + version chip `dev · v0.4.2` (JetBrains Mono 10px, `var(--ink-3)`).
- **Center:** Session ID `session 4f9e1a` (JetBrains Mono 11px, `var(--ink-2)`) + uptime `1h 14m` (JetBrains Mono 10px, `var(--ink-4)`).
- **Right:** WS status pulse (mint dot + "connected", `live-pulse` animation), ingress/egress counters (small mono numerals), and a single `Shutdown` button (terra outlined). No "Stop turn" — explicitly removed.

All right-side chips use `flex-shrink: 0` and `nowrap` to avoid wrapping on narrow viewports.

### 2. Left Rail (`248px` wide)

Top to bottom:

1. **`Audio` label** (JetBrains Mono 10px uppercase, letter-spacing 0.08em, `var(--ink-3)`)
2. **Stacked envelope card** — single white card with two horizontal strips joined by a hairline divider:
   - Strip 1 (mic): 26px-wide left gutter with `↑` arrow (mint), then body with `mic` name + device dropdown (`MacBook Pro Mic`) on top row, oscilloscope meter (22px tall, repeating-linear-gradient vertical lines on `#FBF8F2`, mint bars animated)
   - Strip 2 (tts): same shape, `↓` arrow (blue-2), `tts` name, `Headphones · −12 dBFS`, blue bars
3. **PTT control** — segmented switch (`Push-to-talk` / `Open mic`), then either a `hold Space to talk` bar (blue-mist, terra when held) or an "open mic live" pill (mint-soft)
4. **`Processes` label + count**
5. **Process cards** — one per supervised process (gateway / talker / llm_server). Each card: 8px hairline border, white background, status dot + name + status text + restart button. Selected card has `var(--blue-2)` left border + `var(--blue-mist)` background.

### 3. Center — Turns Panel

Top to bottom inside `.turns-wrap`:

#### 3a. Timeline strip (atop turns list)

- 48px min-height, `var(--panel-2)` background, bottom hairline rule
- Inside: padding `4px 10px 4px 12px` (left inset 12px puts the lane labels comfortably away from the panel edge)
- The lane labels (User / Talker / Tool / Reasoner) sit in a 72px-wide gutter with 6px additional left-padding
- Lanes are 16px tall each (shorter than the inspector's 24px lanes)
- Linear single timeline — newest = right edge, oldest = left edge. One typed block per event positioned by absolute time-from-now. Block colors: user = `var(--blue-soft)`, talker = `var(--blue)` with diagonal stripe when streaming, tool = `var(--plum)`, reasoner = `var(--blue-soft)` with `var(--blue-2)` left rule.
- Reasoner blocks must **not** show framework strings (no `openclaw` etc.) — that was a placeholder.

#### 3b. Search + Export bar

- Search input (full-width, hairline border, focus → `var(--blue-2)` border)
- Right: turn count summary (`7 turns · 4 assistant · 3 user`) and a small `Export` button (no chevron — explicitly removed).

#### 3c. Turn rows list

- **Newest on top**, pinned. Each turn is a full row in `.turns-list`.
- **Streaming (in-flight) turn:** lighter `#FBF8F2` background, no left banner, no "streaming" tag. Just a colorless text pulse (gray ↔ black) on the body text. Sentence body uses skeleton shimmer for not-yet-arrived fields.
- **Settled turns:** `#FBF8F2` default, `var(--paper-2)` on hover, full-opacity text. Non-highlighted (when a different turn is selected) turns dim to ~28% opacity.
- **User vs assistant rows:** clear chip on the left (`User` mint pill / `Kaguya` blue pill), inline transcript text, full transcript (transcripts per turn are short), timestamp (JetBrains Mono 10px) on the right.
- **Click a turn → selected**; click the same turn again → deselected → inspector returns to live monitor view.

### 4. Right Inspector (`360px` wide)

Two distinct states:

#### 4a. Live monitor (no turn selected — default state)

- **Latest / streaming turn card** at top — shows the most recent turn with `streaming` or `latest` pin (mint-soft / paper-2). Skeleton-shimmer placeholders for fields that haven't arrived yet (a `.skel` span with shimmering gradient, or `.skel-row` for whole rows).
- **Event timeline (network-style)** below — populates as events arrive. Same four-lane structure as the strip, but lanes are 24px tall and the gutter is wider.
- (Audio I/O is **not** duplicated here — it now lives only in the left rail.)

#### 4b. Selected turn

- Turn header (id, role, duration, tags)
- Sentence list — full transcript split by sentence with per-sentence timestamps
- Event list — per-event timeline with icons (`started` ▷, `delegate` ⤳, `tool` ⚙, `reasoner` ✦, `bargein` !, `complete` ◆)
- Reasoner block (when `delegated`) — pale-blue inset card. Header says only `reasoner · task <id>` (no framework name). Steps listed with relative timestamps.

### 5. Bottom — Logs Panel (collapsible)

Top to bottom inside `.logs`:

1. **Prompt bar** (always visible, even when collapsed) — `›` prefix (blue-2, mono 600), text input (`Send a text prompt to Kaguya…` placeholder), `⏎ send` hint, blue-2 `Send` button. Hitting Enter submits. Disabled when input is empty.
2. **Log toolbar** — `LOGS` label, source filter chips (`all` / `gateway` / `talker` / `llm_server`), level filter chips (`INFO+` / `WARN+` / `ERROR`), Clear / Save .log buttons, and a collapse toggle (`▾` caret + `⌃\`` hint).
3. **Log rows** (hidden when collapsed) — JetBrains Mono 11px, grid columns `timestamp | source | level | message`. **Top-to-bottom auto-scrolling** like a terminal. Scroll up → auto-scroll pauses; scroll back to bottom → re-arms.

When collapsed, the row shrinks to `min-content` so it ends exactly at the bottom of the log-toolbar — no overshoot.

## Interactions & Behavior

- **Click a turn row** → selects it (inspector shows turn detail). Click same row → deselects → inspector returns to live monitor.
- **Click a process card** → selects it (could later show process detail in inspector; currently no-op).
- **Hold `Space`** → PTT armed (bar goes terra-soft, dot pulses).
- **Toggle Open mic** in PTT segmented control → "always-listening" pill (mint-soft, live pulse dot).
- **`⌃\`` (Ctrl+Backtick)** → toggle logs panel collapse.
- **Type in prompt bar + Enter** → send text prompt to gateway as an ingress `TextMessage`.
- **Scroll up in logs** → auto-scroll pauses. Scroll back to bottom → re-arms.
- **Streaming animations** to keep:
  - Text-pulse (gray ↔ black) on streaming turn body
  - Skeleton shimmer (`skel-shimmer` keyframe, 1.6s) on not-yet-arrived fields
  - Live pulse (`live-pulse` keyframe, 1.4–2s) on WS status dot, PTT held dot, open-mic live dot
  - Meter bars (`bar` keyframe, 1.2s ease-in-out, staggered delays for adjacent bars)
  - Network blocks for in-flight turns get a diagonal-stripe pattern overlay

## State Management

Minimum state the new console needs (in addition to what's already in the codebase):

- `selProc: string | null` — selected process name (left rail)
- `selTurn: string | null` — selected turn id (`null` = live monitor)
- `logsCollapsed: boolean` — logs panel state
- `promptInput: string` — text prompt being composed
- `pttMode: 'ptt' | 'open'` — push-to-talk vs always-on
- `pttHeld: boolean` — currently held
- `autoScrollLogs: boolean` — derived from scroll position
- `searchQuery: string` — turn list search filter
- `logSrc: 'all' | 'gateway' | 'talker' | 'llm_server'`
- `logLvl: 'INFO+' | 'WARN+' | 'ERROR'`

Live data should come from the existing WS connection (`ws.ts`) — subscribe to:
- `IngressMessage` / `EgressMessage` counters
- `TranscriptDelta` for in-flight turn body
- `AudioOutput` frames → AnalyserNode → meter bar heights for tts strip
- `MediaStream` from `getUserMedia()` → AnalyserNode → meter bars for mic strip
- `TurnLifecycle` (started / complete / interrupted) for turn list updates
- `ToolCall`, `Reasoner*`, `Delegate*` for event blocks

Bound buffers (configurable):
- Audio ringbuffer: cap on samples (aggressive)
- Metadata ringbuffer: cap on turn history (aggressive)
- Log buffer: cap on lines (less aggressive — terminal feel needs scrollback)

## Design Tokens

### Colors

Defined as CSS custom properties on `:root` in `Console.html`. Copy these into a tokens module / Tailwind config / wherever your codebase keeps design tokens.

```
/* paper / neutrals (warm) */
--paper:        #F1ECE2
--paper-2:      #E8E2D6
--paper-3:      #DBD3C3
--panel:        #F7F3EB
--panel-2:      #FBF8F2
--cream:        #FBF6E8
--hairline:     #E2DBCC
--rule:         #C8C0AE

/* ink (cool) */
--ink:          #2A3645
--ink-2:        #4A5567
--ink-3:        #7B8696
--ink-4:        #A9B2BF

/* blues (primary accent) */
--blue:         #7B92AE
--blue-2:       #5A7595
--blue-soft:    #9FB4CC
--blue-pale:    #C9D3E0
--blue-mist:    #E2E8EE

/* support */
--mint:         #9CB8A6   /* user / success */
--mint-soft:    #D5E1D7
--terra:        #B97862   /* danger / held */
--terra-soft:   #ECD8CE
--plum:         #8E7A95   /* tool */
--plum-soft:    #DAD2DF
--amber:        #B58D52   /* warn */
--amber-soft:   #ECDDC2
```

### Typography

- **UI:** `'Inter', system-ui, sans-serif`
- **Mono / data:** `'JetBrains Mono', monospace`
- **Display / wordmark:** `'Newsreader', serif` (italic for accents)

Scale:
- Wordmark / display: 16–22px Newsreader 500
- Body: 13px Inter 400
- UI strong: 12–13px Inter 500/600
- Labels: 10px JetBrains Mono 600 uppercase, letter-spacing 0.08em
- Captions: 9–10px JetBrains Mono 400, `--ink-3` or `--ink-4`
- Log rows: 11px JetBrains Mono 400, line-height 1.55

### Spacing

4 / 6 / 8 / 10 / 14 / 16 / 24 px steps. Most card padding is 8–14px. Gap inside flex rows is 6–10px.

### Border radius

- Inputs / chips / small buttons: `6px`
- Cards: `6–8px`
- Pills: `999px`

### Hairlines

- `1px solid var(--hairline)` for in-card dividers
- `1px solid var(--rule)` for major section rules (top bar bottom, between rows of the grid)

### Animations

```css
@keyframes live-pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.4; } }
@keyframes skel-shimmer {
  0%   { background-position: 100% 0; }
  100% { background-position: -100% 0; }
}
@keyframes bar { 0%, 100% { transform: scaleY(0.3); } 50% { transform: scaleY(1); } }
```

## Assets

No image assets. All UI is CSS + unicode arrows (`↑ ↓ ⏎ ▷ ⤳ ⚙ ✦ ◆ ▾`) + Google Fonts (Inter, JetBrains Mono, Newsreader).

In `index.html`, add the Google Fonts preconnect + stylesheet link the same way `Console.html` does.

## Implementation Notes / Gotchas

- **Mic capture is already wired in production** — don't redesign that path. Just hook the existing MediaStream into a meter AnalyserNode for the left-rail mic strip.
- **Speaker notes / status: `Stop turn` is explicitly removed** from the top bar.
- **No tabs row** above the turns panel — that was removed. The center panel is single-purpose.
- **Reasoner labels carry no framework string** — placeholder text only, do not surface it.
- **The Export button has no chevron** — single text label only.
- **Audio I/O lives in the left rail only** — no duplicated meters in the inspector live-monitor.
- **The chosen audio module is Option 5** (envelopes) from `Audio Devices.html`. The other four options are reference, not pending decisions.

## Open Questions for the Developer

- Buffer caps (audio / metadata / logs) should be configurable — wire them up to whatever supervisor config layer makes sense (`supervisor.json` extension? Vite env? in-app settings panel?).
- The prompt bar currently has a no-op submit. Wire it to send an ingress `TextMessage` via `ws.ts`.
- Search in the turns panel is currently UI-only — implement full-text search across `turn.body` + transcript sentences.
- Process restart buttons (left rail) need to call the supervisor plugin's restart endpoint.
