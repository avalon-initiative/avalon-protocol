# SDK language support

One table, kept as the single source of truth for "is there an Avalon SDK
for my language?" — [`README.md`](README.md) and
[`architecture/sdk.md`](architecture/sdk.md) link here rather than keeping
their own copies of this list.

## Supported today

| Language | Package | Status | Notes |
|---|---|---|---|
| **Rust** | `avalon-sdks` repo's `languages/rust/` | **Reference implementation** | Built alongside `backend-server` itself, so it's the most complete SDK and the one every other language is checked against. Both `Session` (integrator) and `AccountSession` (first-party) surfaces. See [`rust/README.md`](rust/README.md). |
| **C#** | `avalon-sdks` repo's `languages/csharp/` (NuGet `Avalon.Sdk`) | **Fully supported** | The flagship external, developer-facing SDK — targets netstandard2.1 for Unity. Mirrors the Rust surface, including `AccountSession`, with one deliberate gap: no WebAuthn-ceremony-driving registration/login (no ceremony library available for this SDK's actual Unity/native audience). See [`csharp/README.md`](csharp/README.md). |
| **TypeScript** | `avalon-sdks` repo's `languages/typescript/` (`@avalon-initiative/protocol-sdk`) | **Fully supported** | Real and shipped, browser-facing — drives a real WebAuthn ceremony (unlike C#). A self-contained package/`avalon-hub/apps/hub`. What `avalon-hub/apps/hub` runs on, so it's exercised by a real production frontend, not only its own test suite. See [`typescript/README.md`](typescript/README.md). |

**Status key:** *Reference implementation* — the language the server and
protocol are developed against first; every other SDK is checked against
its behavior. *Fully supported* — real, tested, live-verified, safe to
build a game/app/service on, with any known gaps stated explicitly rather
than silently. Nothing here is ever labeled "supported" on the strength of
a design doc alone.

## Not yet supported

Listed so "is there an SDK for X?" has a real, honest answer instead of
silence. **None of these are on any planned timeline.** Consistent with
this whole project's posture (see [`README.md`](README.md)): a language
gets built only once an actual integration needs it, not speculatively
ahead of demand — and once SDK generation moves onto a shared
protobuf/schema definition, adding a language here may become a much
smaller lift than hand-writing one from scratch is today.

### General-purpose / backend

| Language | Typical use case if built |
|---|---|
| Go | A server-side game backend, the same role the C# SDK plays for a client. |
| Python | Tooling, bots, data/analytics integrations, server-side backends. |
| C++ | Native engines without a higher-level scripting layer (e.g. custom/in-house engines, or Unreal C++ gameplay code directly rather than through Blueprints). |
| Java / Kotlin | A native Android game client. Today that audience has no better option than the TypeScript SDK inside a WebView, or a direct wire-protocol integration. |
| Swift | A native iOS game client — the same gap as Java/Kotlin, for iOS. |
| PHP | Web-backend integrations (e.g. an existing PHP game-services backend). |
| Ruby | Same category as PHP — server-side/web integrations. |
| Dart | A Flutter-based client, mobile or desktop. |
| C | The lowest common denominator for engines/runtimes with no other binding available; would likely double as the basis other native bindings FFI against, rather than being hand-written independently. |

### Game-engine scripting languages

| Language | Engine | Typical use case if built |
|---|---|---|
| GDScript | Godot | A Godot game talking to Avalon directly from game code, the same role the C# SDK plays for Unity. |
| Lua | Many engines/runtimes (e.g. Roblox's Luau, Defold, custom engines) | Scripting-layer integration for engines that expose Lua as their primary or only scripting surface. |

This table grows as languages are added, not as they're merely requested —
see [`README.md`](README.md)'s "Why one folder, not one per language"
section for why a new language is likely to mean a new subfolder here
rather than a new top-level project.
