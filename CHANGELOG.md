# Changelog

## [Unreleased]

## [0.1.2] - 2026-11-27

### Added

- **Conversation tabs.** The agent replies in the tab where you messaged it. Each tab has its own chat context.
- **Clear chat context.** The owner can send `/clear` or `/clearcontext` to start fresh in the current tab and keep its transcript.
- **ZIP attachments.** Send a ZIP of supported text files with your question. The agent tells you when it skips entries or shortens the text to fit the model.

### Fixed and improved

- The default prompt gives the agent a consistent assistant role across generated contact names.
- Text attachments keep their extensions when filenames are shortened or sanitized. A duplicate delivery preserves the file the agent already received.
- Hermes now gets the text from supported files and ZIPs along with your question.
