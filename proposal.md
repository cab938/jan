# Codex App Server Integration Proposal (POC)

## Summary

This proposal adds **Codex App Server** support to Jan using a minimal-change approach:

- Implement a **Codex App Server shim** that exposes an OpenAI-compatible API to Jan.
- Add a **new Jan extension + provider** for `codex-app-server`.
- Introduce a **tool-mode dropdown** in the provider settings, plus **disabled checkboxes** that explain the implications of each mode:
  - “Codex tools auto-approved”
  - “Jan tool execution”

This preserves Jan’s existing provider UX while allowing a phased path toward deeper harness integration.

---

## Goals

- Provide a **POC** that can ship quickly without large UI or chat pipeline rewrites.
- Keep Jan’s existing providers and flows intact.
- Allow **future refactor** toward full Codex App Server event handling.

## Non-Goals (POC)

- Full JSON‑RPC event‑level UX (items, approvals, agent plans).
- Deep tool approval flows within Jan for Codex-native tool calls.
- Thread/turn/item UI parity with Codex web.

---

## Architecture Overview

### 1) Codex App Server Shim (Local, Bundled)

Purpose: Convert Codex App Server JSON‑RPC into OpenAI‑compatible `/v1/models` and `/v1/chat/completions` for Jan.

Responsibilities:

- Spawn/maintain a local Codex App Server connection.
- Implement `/v1/models` by calling `model/list` and mapping to OpenAI model listing.
- Implement `/v1/chat/completions` (SSE) by translating Codex event streams into ChatCompletion deltas.
- Respect **tool mode**:
  - **Jan tools mode**: emit OpenAI tool calls (if supported) and do NOT execute Codex tools internally.
  - **Codex tools mode**: do NOT emit tool calls; Codex tools are executed internally and results are folded into assistant text.
- Maintain **thread continuity** using a stable thread id passed from Jan (minimal Jan change).

### 2) Jan Extension: `codex-app-server`

Purpose: Provide provider metadata, settings, and lifecycle for the shim + app server.

Implementation path:

- Add a new extension in `extensions/codex-app-server-extension/` (TypeScript, like other engines).
- Extend `AIEngine` to:
  - Provide `provider: 'codex-app-server'`
  - Register settings (see below).
  - Expose model list to Jan (either from shim `/v1/models` or via a shim-side cache).

This makes the provider appear in Jan’s Providers UI alongside other engines, with settings rendered by the existing `DynamicControllerSetting` path.

---

## UI/Settings Proposal

### Provider Settings (Codex App Server)

Add these settings via `SettingComponentProps` in the extension:

1. **Tool Mode** (dropdown)
   - `Jan tools (default)`
   - `Codex tools (auto-approved)`

2. **Explanation Checkboxes** (disabled)
   - `Codex tools auto-approved` (checked only when tool mode is Codex)
   - `Jan tool execution` (checked only when tool mode is Jan)

3. **App Server / Shim Settings**
   - Path to shim binary (bundled)
   - Auto-start on app launch (checkbox)
   - Codex App Server binary path (explicit setting)
   - Optional: diagnostics log path

This keeps the tool‑mode implications explicit and reduces user confusion.

---

## Minimal Jan Code Changes

We will keep changes small and localized:

1. **Thread continuity header**
   - Pass the Jan thread id to the shim in a request header (e.g. `x-jan-thread-id`).
   - Requires a minor change in `ModelFactory` or `CustomChatTransport` to allow request‑scoped headers.

2. **Tool mode gating**
   - If provider is `codex-app-server` and tool mode is `Codex`:
     - Disable tool availability and tool call execution in Jan.
   - If provider is `codex-app-server` and tool mode is `Jan`:
     - Preserve existing Jan tool behavior.

This prevents double-execution and keeps the POC safe.

---

## Tool Mode Behavior Matrix

| Mode | Jan sends tool schemas | Jan executes tools | Codex executes tools | Notes |
|------|-------------------------|--------------------|----------------------|------|
| Jan tools | Yes | Yes | No | Default, mirrors current Jan behavior |
| Codex tools | No | No | Yes (auto-approved) | Results folded into assistant text |

---

## Implementation Steps (POC)

1. **Shim (new project)**
   - Implement `/v1/models` and `/v1/chat/completions` (SSE).
   - Implement Codex App Server JSON-RPC client.
   - Add tool-mode logic.
   - Add thread id routing.

2. **Jan Extension**
   - Add `codex-app-server` extension in `extensions/`.
   - Register settings + default values.
   - Expose models to `EngineManager` so provider appears in UI.

3. **Jan UI Minimal Changes**
   - Read tool mode from provider settings.
   - Gate tool sending/execution based on mode.
   - Add per‑request header with thread id.

---

## Risks / Tradeoffs

- **Lossy mapping**: Codex item-level events are collapsed into OpenAI chat deltas.
- **Auto-approve tooling**: Codex tool calls bypass Jan’s approval UI in Codex mode.
- **Dependency on shim**: Operational complexity increases (process lifecycle, logs, updates).

These are acceptable for a POC and can be addressed in later phases.

---

## Future Enhancements (Post-POC)

- Replace shim with native Codex transport in Jan.
- Render Codex items (plans, diffs, approvals) in the UI.
- Unified approval UI across Jan tools and Codex tools.
- Expand thread metadata to support Codex‑specific state.

---

## Open Questions

- Should we store tool mode per provider or per thread? (POC: per provider)

---

## Naming

We will use **`codex-app-server`** consistently for the provider and extension, since that is the concrete runtime being integrated.

---

## POC Scope Clarifications

- **Local-only**: POC targets the local Codex App Server only.
- **Bundled shim**: The shim ships with Jan (no external install step).
- **Tool mode**: Stored per provider for now; can be revisited per-thread later.
- **No bundled Codex binary (yet)**: The Codex App Server binary is user-supplied.

---

## Binary Discovery & Packaging

Because Jan’s packaging (especially on Linux) can sandbox PATH resolution, we should not rely solely on `PATH`.

POC behavior:

- Primary: **Explicit Codex App Server binary path** in provider settings.
- Fallback: If empty, try `PATH` lookup with a clear error if not found.

This keeps the POC workable in a jailed environment and avoids fragile PATH assumptions.

---

## Logging

Extension and shim logging should follow the existing engine pattern (console + app logs), matching how other extensions (e.g. MLX) log today.

---

## Implementation Checklist (Acceptance Criteria)

1. **Provider & Extension**
   - `codex-app-server` provider appears in Providers list.
   - Settings are rendered via `DynamicControllerSetting`.
   - Defaults: tool mode = **Jan tools**, auto-start = off.

2. **Settings UI**
   - Dropdown: `Jan tools (default)` / `Codex tools (auto-approved)`.
   - Disabled checkbox indicators update when dropdown changes:
     - “Codex tools auto-approved” checked only in Codex mode.
     - “Jan tool execution” checked only in Jan mode.
   - Clear disclosure text matches the initial wording.

3. **Binary Discovery**
   - Setting for **Codex App Server binary path** exists.
   - If unset: attempt PATH lookup; if not found, surface a clear error.
   - Works under Linux sandbox packaging with explicit path.

4. **Shim + App Server Lifecycle**
   - Shim starts/stops via extension or Tauri command.
   - Shim maintains a stable connection to local Codex App Server.
   - Logs are written in the same way as other engines.

5. **Model Listing**
   - `/v1/models` returns Codex models (via `model/list`).
   - Refresh models in Provider UI works for `codex-app-server`.

6. **Chat Completions (Streaming)**
   - `/v1/chat/completions` works with SSE.
   - Responses render in Jan without UI changes.
   - Thread continuity: Jan passes a stable thread id header.

7. **Tool Mode Gating**
   - **Jan tools mode**: Jan sends tool schemas and executes MCP/RAG; shim does not execute Codex tools.
   - **Codex tools mode**: Jan does not send tools and does not execute tool calls; Codex tools run internally with auto-approve.
   - No double-execution occurs in either mode.

8. **Regression Safety**
   - Existing providers (OpenAI, Anthropic, llama.cpp, etc.) behave unchanged.
   - `codex-app-server` can be toggled on/off without breaking other providers.
