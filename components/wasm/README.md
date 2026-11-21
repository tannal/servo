# Native WebAssembly components in Servo

This document describes the design of Servo's experimental native WebAssembly
runtime.  It is intentionally written against the same primitives that browsers
already use for JavaScript (`HTMLScriptElement`, the fetch pipeline, the event
loop and `EventTarget`), but replaces the JavaScript glue layer with the
WebAssembly Component Model's typed linking and canonical ABI.

The implementation lives in:

- `components/wasm` – process-wide Wasmtime engine, component cache, HTTP client.
- `components/script/wasm_host.rs` – host implementation of `servo:dom` and
  `servo:dom/fetch`, component lifecycle and event dispatch.
- `components/script/wit/servo_dom.wit` – the single source of truth for the ABI.
- `components/script/dom/html/scripting/htmlscriptelement.rs` – the
  `<script type="wasm-component">` loading path.
- `components/script/dom/event/eventtarget.rs` – native listener plumbing.

## 1. What browsers already do, and what we reuse

Browsers expose WebAssembly through the JS API: a module is fetched as
`application/wasm`, compiled, linked against a JavaScript `importObject`, and
instantiated as a `WebAssembly.Instance`.  The ESM integration proposal keeps
the same linking model but lets modules participate in the module graph
(`<script type="module" src="app.wasm">`, `import { x } from "./app.wasm"`).
CSP `script-src`, CORS, integrity metadata and MIME checks apply because the
artifact travels through the same script/module loading machinery as JS.

Servo already has the pieces that matter:

| Browser concept | Servo implementation reused here |
| --- | --- |
| script fetch, CORS, CSP, integrity | `HTMLScriptElement` + classic script fetch |
| module evaluation ordering | `HTMLScriptElement::execute` |
| task sources / event loop | `TaskManager` + `TaskSource::to_sendable` |
| event listeners and dispatch | `EventTarget` + `CompiledEventListener` |
| realm / incumbent global | `enter_auto_realm` |
| per-document lifetime | `GlobalScope` / pipeline |

The new piece is the **host linker** and a native, typed ABI that replaces the
JavaScript import object and `wasm-bindgen` glue.  This follows the direction
described in Mozilla's "Making WebAssembly a first-class language on the web"
and the Component Model's host-linking model.

V8 (`src/wasm/wasm-js.cc`), SpiderMonkey (`js/src/wasm/WasmJS.cpp`) and
JavaScriptCore (`Source/JavaScriptCore/wasm/js/`) all use the same shape:
fetch → compile → read imports → instantiate → call exports.  SpiderMonkey's
`AsyncInstantiateTask` and V8's streaming instantiation are examples of the
"do compilation off the main thread, resolve on the event loop" pattern that
this runtime mirrors for HTTP requests.

## 2. Artifact and loading

The first version accepts a **core module built for
`wasm32-unknown-unknown` and componentized by `cargo component`**.  The
resulting file is a Component Model component (its leading bytes distinguish it
from a core module), so the host can be strict about the artifact type.

```html
<script src="chat.wasm"></script>
<script src="railwind.wasm"></script>
```

No JavaScript, no `type` attribute and no application CSS are required.  A
downloaded classic script is recognised as a component by its magic bytes
(`\0asm` followed by the Component Model version word).  The explicit
`type="wasm-component"` spelling remains supported for pages that want to be
unambiguous.

The fetch path is Servo's classic-script fetch, so the bytes arrive with the
same origin/CORS/CSP treatment as a classic script.  Component scripts are
executed independently and may complete in any order; the Tailwind integration
below is explicitly order-independent.  A production version should require an
`application/wasm` (or explicit `application/octet-stream`) response and honour
Subresource Integrity.

## 3. Linking and the ABI

The host exposes a small, typed world:

```wit
world app {
  import console;
  import document;
  import fetch;
  export run: func();
  export on-event: func(event: event-data);
  export on-fetch: func(request-id: u32, ok: bool, status: u16,
                        body: list<u8>, error: option<string>);
}
```

`servo:dom/document` exposes `element` as a **resource**.  The guest owns the
handle; dropping it releases the slot in the host's handle table.  This is the
Component Model equivalent of the JS API handing a `Node` wrapper to JS, but
without an intermediate JS object, a `FinalizationRegistry`, or a linear-memory
pointer.

Design choices, and why:

- **Resources, not raw integers.**  A guest cannot fabricate a DOM handle or
  use a stale one; the canonical ABI enforces ownership.
- **Strings and lists cross by value.**  UTF-8 strings and `list<u8>` are
  copied by the canonical ABI.  This is slower than sharing linear memory but
  removes a whole class of aliasing and lifetime bugs, and it does not expose
  Servo's allocator to the guest.
- **Explicit small event payloads.**  `event-data` carries the key, form value,
  checked bit and pointer coordinates.  UIs such as Dioxus need these on every
  keystroke; making them part of the ABI avoids a synchronous round trip into
  the DOM for the common case.
- **No JavaScript realm is required for the guest.**  The host still enters the
  document's realm before touching DOM APIs, exactly as a JS callback would, so
  incumbent/global object assumptions in Servo's bindings hold.

Future versions should support component-to-component linking through the same
host linker.  That is the natural place to plug in `railwind.wasm` as a
separate CSS compiler component rather than linking its core into the app.

## 4. Event loop and asynchronous work

Synchronous exports (`run`, `on-event`) execute on the ScriptThread with the
document's realm active.  The instance is kept alive in a thread-local slot
(the `Store` is not `Send` because it contains `DomRoot`s) and is temporarily
taken out of the slot while the guest is running, which prevents re-entrant
`Store` borrows.

Asynchronous work never blocks the ScriptThread:

1. `servo:dom/fetch` validates the request and returns a request id
   immediately.
2. A dedicated thread performs the HTTP exchange.
3. On completion it queues a `TaskOnce` on Servo's `Networking` task source
   (`TaskSource::to_sendable`), which is the same mechanism used by other
   off-thread network clients.
4. The task re-enters the guest's `on-fetch` export on the ScriptThread.

This is the same shape as the JS API's promise resolution: compilation/IO
happens off-thread and the observable callback runs as a task on the event
loop.  It also means long LLM generations cannot freeze rendering or input.

## 5. Networking

`servo:dom/fetch` is deliberately minimal:

- `http://` only for now (enough for a local Ollama server);
- a 300 s read timeout for long generations;
- `Content-Length` and `Transfer-Encoding: chunked` responses;
- a 64 MiB response cap.

A browser-grade implementation must route this through Servo's `net`/fetch
stack with the document's origin, CORS preflight rules, redirect policy,
credentials, CSP `connect-src`, and streaming.  The current experimental host
is a capability, not a web API: pages should not be able to reach arbitrary
hosts through it.  Before this can ship, it needs an allowlist/preference and
must not bypass the same-origin/CORS checks that `fetch()` is subject to.

## 6. CSS: railwind as a separate component

`railwind.wasm` is loaded by its own `<script>` tag.  It exports
`servo:tailwind/compiler` and is instantiated by the host, not by the
application.  Whenever an application component writes a `class` attribute
through `servo:dom`, the host marks the stylesheet dirty.  After every guest
entry point (`run`, an event, or a fetch completion) and when railwind itself
registers, the host scans the live document for `class` attributes, calls
railwind's `compile-candidates` export, and injects the resulting stylesheet
(preflight + utilities) into a `<style>` element owned by the document.

The flush is synchronous rather than task-queued.  This matters during document
loading: a task queued by a class write can be cancelled or run before the
second component is registered, leaving the page unstyled with no later
opportunity to recover.  Scanning the document also makes the result
independent of which mutation path wrote the class and of whether the app
finished before or after `railwind.wasm`.

This is deliberately host-mediated rather than a direct component-to-component
import:

- the application stays a plain UI component and contains no CSS engine;
- the two scripts can finish loading in either order — class tokens are queued
  until railwind is available, and a flush runs when it registers;
- the same mechanism works for any component or even for DOM mutations made by
  the host itself;
- it mirrors the referenced tailwind integration experiment in Servo's
  experimental branch.

A future version can additionally expose `servo:tailwind/compiler` to
components through the host linker so that a component can ask for CSS
synchronously.

## 7. Virtual DOM bridge

Dioxus 0.6's `WriteMutations` is implemented directly against `servo:dom`:
create/append/insert/remove/replace, attributes, text, event listeners and
template instantiation.  The bridge deliberately keeps mutation ordering
correct (`insert_nodes_after` uses `next-sibling`), because chat-style lists
exercise insert-before/after heavily.  Class attributes written by the bridge
are observed by the host's Tailwind collector; the guest does not need a CSS
engine or a style-injection layer.

Dioxus event payloads are reconstructed from `event-data`:

- `click` → `MouseData`
- `keydown` → `KeyboardData`
- `input`/`change` → `FormData`

No JS value conversion is involved; the data is already typed and lives in the
guest.

## 8. Lifecycle and multi-instance support

Each `<script type="wasm-component">` gets an `app_id` slot.  The id is encoded
into every native event handler id (`<app-id>:<handler-id>`), so events are
routed to the correct component without changing Servo's generic listener
structures.  DOM handles are table entries with a free list; dropping a WIT
resource returns the slot.

The remaining lifecycle work is document teardown: when a pipeline is
destroyed, its component slots and outstanding requests must be cancelled, and
the handle table released.  A production version should also integrate with the
JavaScript GC for hybrid JS + component pages.

## 9. Security model

The experiment intentionally starts from a small ambient-authority surface:

- The guest can only call the interfaces compiled into its world.
- DOM handles are unforgeable resources.
- Networking is a host capability and must eventually be origin/CORS aware.
- Script loading reuses the existing script fetch path, so CSP/CORS apply to
  the artifact itself.
- There is no `eval`, no JS interop and no WASI.

Lessons taken from the JS platform: every host capability needs an explicit
permission and an origin check; a native binding must not become a way to
escape the web security model just because it avoids JavaScript.

## 10. Why not just use the JS API?

The JS API is the right answer when a page already has JavaScript and only
needs to run a module.  It is the wrong answer for the goal of "a Rust/C++/…
application that is a first-class web application":

- every language must ship a JS glue layer (wasm-bindgen, embind, …);
- strings and structs are re-encoded across the boundary;
- the app must ship and understand a JS loader;
- web documentation is written for JS developers.

The Component Model gives a standard, language-neutral artifact and a typed
linker.  The ESM integration proposal and the Component Model are complementary:
ESM integration gives wasm modules a place in the module graph; the Component
Model gives them a typed, sandboxed host interface.  Servo's experiment is a
concrete host implementation of the latter, reusing the former's loading and
event-loop semantics.

## References

- WebAssembly JS API specification:
  <https://webassembly.github.io/spec/js-api/>
- WebAssembly/ES Module Integration proposal:
  <https://github.com/WebAssembly/esm-integration>
- WebAssembly Component Model:
  <https://github.com/WebAssembly/component-model>
- "Making WebAssembly a first-class language on the web":
  <https://hacks.mozilla.org/2026/02/making-webassembly-a-first-class-language-on-the-web/>
- V8 WebAssembly JS bindings: `src/wasm/wasm-js.cc` in <https://github.com/v8/v8>
- SpiderMonkey WebAssembly JS bindings: `js/src/wasm/WasmJS.cpp` in
  <https://github.com/mozilla/gecko-dev>
- JavaScriptCore WebAssembly bindings: `Source/JavaScriptCore/wasm/js/` in
  <https://github.com/WebKit/WebKit>
- Servo script/event-loop machinery:
  `components/script/dom/html/scripting/htmlscriptelement.rs`,
  `components/script/dom/event/eventtarget.rs`,
  `components/script/tasks/`.
