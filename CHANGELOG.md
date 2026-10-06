# Changelog

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
