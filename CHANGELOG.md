# Changelog

## Unreleased

## 0.1.14

- Reduced checkpoint storage: unchanged committed files reuse pinned Git objects instead of copying the repository, while dirty, new and ignored files retain their exact bytes. Projects without usable Git keep standalone snapshots; HEAD, branches and staging remain unchanged.
- Deduplicated and compressed checkpoint conversations in shared archives, reused unchanged local file copies across chats, and removed a chat's checkpoints and Git references when deleting it. Identical image attachments share one cached file; logs rotate at 2 MiB.
- Added a Copy button to fenced code blocks. Mouse selections copy original code without borders or display wrapping, preserving tabs, whitespace, Unicode and literal code characters.
- Aligned rewind session restoration with the GUI: restore metadata and context, reset agent/helper/LSP state, cancel stale title generation and keep already-spent tokens accounted for. Checkpoint listing, preview and restore run in the background with terminal progress.
- CLI reads existing GUI checkpoints; new checkpoint formats 2–3 require this CLI and are not readable by older CLI or current GUI versions. Existing file snapshots remain supported. New Git-backed checkpoints require their repository's saved Git data.
- Release validation now includes terminal regressions for rewind, code copying, long code blocks and attachment caching before building and publishing platform archives.
- Updated saved-transcript regression checks to the shared GUI/CLI history format, fixing CI failures left by the previous release's format migration.

## 0.1.13

- Added the built-in `grill_me` questionnaire tool and `/grill-me [topic]` command, with independent questions, recommended answers, free text and partial-answer cancellation. The tool is available in Plan mode and excluded from read-only helper agents.
- Matched reasoning effort choices to the active model and provider metadata across compatible Chat Completions, native OpenAI Responses and Anthropic Messages connections. Added namespaced and dated model aliases; unsupported effort values are omitted from requests.
- Unified project session history with Takiza GUI, reading legacy and tagged transcript formats while preserving metadata. `/resume` hides empty and unreadable sessions without deleting files; user and assistant transcripts remain available in either client.
- Release publication now waits for tests and all six platform archives to finish.

## 0.1.12

- Provider creation/editing now opens a searchable model catalog using the draft connection's optional API key or key environment variable. Anonymous catalogs work without credentials; authorization failures are distinguished from unavailable catalogs, with manual model entry preserved. Cancelling the model picker discards the draft without saving it.

- Added `takiza update` with installation-channel detection: npm updates preserve their prefix/project, standalone binaries use verified GitHub release archives, and Cargo installs retain their original source/root. npm and standalone releases check for automatic updates daily and install after normal exit; `auto_update: false` in global or workspace JSON disables automatic updates. Source builds and temporary npx installations are excluded.

- Replaced unreliable keyless Bing RSS search with public Brave Search HTML. Added `TAKIZA_SEARCH_PROVIDER=free` to force keyless search without removing configured API keys. Only organic result cards are parsed; unrecognized/challenge pages fail explicitly. Clarified that `web_search` is an ordinary Takiza function independent of the model provider’s native browsing API.

## 0.1.11

- Current API-key connections are treated as third-party providers: `/mode` explains that Takiza mode selection is unavailable, and `/usage` and `/quota` direct users to their provider dashboard instead of showing mock quotas. Settings and status show the same connection type. Saved MoA settings run in Manual for now. Evening Irkutsk is preserved as a hidden theme outside the public theme list.

- MCP connection, discovery and tool-call errors retain the underlying provider or transport reason, with credential redaction and bounded terminal-safe text. Connection and discovery timeouts are reported explicitly instead of suggesting login for every failure.

- Added an application-wide `context_window` override, proactive history compaction and provider-scoped cap warnings. Unsupported values are clamped for runtime use without rewriting the configured limit; unknown model caps use the requested value until a provider reports a lower maximum.

- Added saved provider profiles with independent credentials, API types, custom HTTP headers, model catalogs and remembered model selections. Existing single-provider settings migrate to a default profile.
- Added native OpenAI Responses and Anthropic Messages transports alongside compatible Chat Completions, including streaming, tools, images, context summaries and conversation titles. Provider continuation items survive saved sessions.
- Model selection loads the active provider catalog in the background, searches readable names and IDs, and supports manual models when discovery is unavailable. Added Russian and Chinese UI translations and protocol/TUI regression coverage.
- Fixed visible ANSI reset fragments in provider information panels, including `/usage` and the mode notice opened from `/status`.

## 0.1.10

- Show the percentage remaining before automatic compaction only when 10% or less of the context window remains; the token counter stays visible.
- Windows checkpoints detect launches from the user profile root before scanning protected application data. A translated message explains how to enable file recovery by starting Takiza in a project directory; chat and tools remain available.

## 0.1.9

- New configurations include Exa MCP for keyless web search and page fetching. The server is an editable `mcpServers.exa` entry in `config.json`, with only `web_search_exa` and `web_fetch_exa` enabled.
- Existing configurations are preserved, including missing or empty MCP server maps. Removing the default Exa entry remains persistent; it is not added back when preferences are loaded or saved.
- Documented the default server, how to disable it, and how to add it to an existing configuration. Added regression coverage for generated configuration and saved MCP settings.

## 0.1.8

- Added Auto, Plan and Allow-all execution modes, switchable with Shift+Tab or `/permissions`. Modes persist across launches; Plan enforces read-only tools at runtime, while legacy auto-approval settings remain supported.
- Added Explore and Reviewer subagents with isolated conversations, read-only tools, two concurrent helpers, bounded execution, cancellation and session token accounting. `/agents` lists helpers, shows reports and cancels tasks.
- Added a workspace access prompt with remembered trusted directories.
- Added a bundled image logo and `/logo <path|reset>` for custom PNG, JPEG, WebP and GIF artwork. Supported terminals use Kitty graphics, inline PNG or SIXEL; other terminals retain colored text artwork.
- Fixed Russian and Simplified Chinese translations for execution modes, active badges, queued prompts, checkpoints, session messages, subagent status, integration management and agent errors. Queue identities and machine-readable statuses remain independent of the interface language.
- Kept the latest answer visible when opening skill completions on short terminals.
- Preserved the terminal's normal exit output when launching through npm, while retaining cleanup after signal termination.
- Expanded documentation and regression coverage for execution modes, subagents, workspace trust, logos and localization.

## 0.1.7

- Configurable MCP services and language-server tools share the agent runtime, with interactive connection management, cancellation, approval for external actions, and diagnostics for the current document version.

- Terminal tabs show `Chat title — Takiza Code`, updating when a title arrives or a chat is opened; empty chats show `Takiza Code`.

- Shell commands use closed stdin so interactive programs cannot consume TUI keys. Cancellation covers output draining and kills the Unix process group (Windows uses taskkill /T), preventing npm/npx descendants from keeping the agent stuck on inherited pipes. Direct `!` commands also accept Esc to stop.

- Active responses save the visible transcript once per second, including streamed text and tool activity, so restarting during a turn preserves recent output. Session JSON replacement is atomic; unexpected agent-task failures become visible errors instead of silently returning to the prompt.

- User messages have a blank line above them in live output and restored chat history.

- Built-in file writes unwrap a single duplicated argument object only when both paths match exactly and its inner content is already a string. Ambiguous objects, extra fields and custom schemas remain rejected; type errors describe known nested field types without exposing values.

- File-write descriptions and repair requests explain how to encode JSON documents as string content, with a valid escaped example. Object/array content remains rejected rather than being silently converted into a file.

- Tool validation reports the failing JSON field and its expected/actual type without exposing argument values. Repair requests explicitly state that field order is irrelevant, preventing retries that merely reorder valid field names.

- Context compaction processes larger provider-aware chunks with visible percent/part progress, 45-second request timeouts and a three-minute overall deadline. Overflow reduces chunk size; cancelled, timed-out and output-truncated summaries preserve the original context. NVIDIA GLM-5.3-Flash summaries use low reasoning with an 8K output allowance.

- Output-limited reasoning and truncated tool calls receive one retry requesting concise reasoning and smaller actions. Truncated calls are never executed or schema-repaired; repeated exhaustion reports the output limit instead of repeating the same request.

- Terminal shutdown keeps raw mode enabled while delayed mouse reports drain, preventing escape sequences from being echoed or passed to the shell after exit.

- The context footer shows approximate percent headroom until compaction, reserving the same 8K output allowance as agent requests. Unknown usage or limits omit the percentage; narrow terminals use a shorter label.

- Explicit context caps in provider overflow errors update the footer and persist for seven days, scoped to endpoint, model, credentials and proxy. Larger metadata cannot overwrite a learned cap; ambiguous errors do not invent one.

- Context limits use only the selected provider's model and deployment metadata. If several caps are reported, the smallest applies; unsupported metadata leaves the maximum unknown. No model catalog or trained-window fallback is used.
- The prompt footer shows current/maximum context (e.g. `32K/128K`) before the chat title, with a continuous green/yellow/red fill gradient. The latest request's usage is persisted separately from cumulative billing; model limits are fetched in the background and cached. Unknown values show `?`, and compaction invalidates the old context size.

- Tool schemas with an explicit object/property schema send root `anyOf`, `oneOf` and `allOf` constraints as description instructions for compatibility with Claude gateways. Full schemas still validate calls locally, including the file/project alternatives in `lsp_symbols`.

- Long chats cache rendered messages and tool groups, reuse unchanged Markdown layout, and copy only visible rows when scrolling or repainting. Menu backgrounds reuse the same layout.

- Streamed assistant text survives tool calls and tool dialogs; separate reasoning steps remain in history, and closing `/tools` restores the conversation.

- Invalid tool calls are repaired using explicit rejected call results and parameter schemas; repeated malformed streamed calls fall back to a cancellable non-streaming request. No missing required arguments are fabricated.

## 0.1.6

- Context overflow triggers automatic context compaction; `/compact` summarizes model memory manually while preserving the visible chat. Compaction supports cancellation and English, Russian and Chinese interface messages.
- Web searches display `websearch` instead of `command`.
- Opening or exiting an unused chat no longer saves an empty conversation; the first submitted prompt starts the session.
- Exiting a session prints persistent usage statistics and an exact resume command; `--session <id>` resumes a saved chat.
- Automatic checkpoints no longer expire after 10 seconds; they remain cancellable.
- Rewind restores the checkpoint's full original prompt to the input field for editing and resubmission.

## 0.1.5

- Fixed Windows startup failing with `Initial console modes not set` by resetting mouse capture only after its console mode has been initialized.

## 0.1.4

- English, Russian and Simplified Chinese interface, with language selection in onboarding and `/config`.
- Expanded theme selection, including Evening Irkutsk with animated snowfall.
- Shared settings panels preserve the conversation and reduce flicker.
- Word wrapping and styled continuation lines in prompts, questions and answer review.
- Successful file edits and writes show compact diffs; Ctrl+O expands them, with theme-aware added and removed row backgrounds.
- Theme-aware bold user messages, persistent busy indication and response duration/token statistics.
- Updated onboarding and user agreement layout, and improved terminal cleanup.
- Web search in the normal tool loop, plus more robust streamed tool-call argument handling.
- Revised agent instructions and skill handling.
