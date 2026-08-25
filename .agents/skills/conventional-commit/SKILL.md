---
name: conventional-commit
description: Create, review, or use Git commit messages that conform to the Conventional Commits 1.0.0 specification. Use when asked to suggest a commit message, validate one, or commit changes with Conventional Commits formatting.
---

# Conventional Commit

Produce a commit message that accurately describes the changes and conforms to [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/#specification).

## Inspect the change

Before choosing a message, inspect the relevant diff:

- For a message describing staged changes, use `git diff --cached` and verify the staged file set with `git status --short`.
- If nothing is staged and the user only wants a suggestion, inspect `git diff` and clearly say the message describes unstaged changes.
- If the user asks to commit, stage only changes that are clearly in scope. Do not include unrelated user changes.
- If the diff contains multiple independent changes, recommend separate commits when practical.
- Describe only the final state represented by the diff. Do not mention intermediate implementations, failed attempts, temporary states, reverted approaches, or the sequence of work unless that history remains materially relevant to the final change.

## Format

Use this structure:

```text
<type>[optional scope][optional !]: <description>

[optional body]

[optional footer(s)]
```

Apply these rules:

- Start with a type, optionally followed by a noun scope in parentheses, optionally followed by `!`, then `: `.
- Use `feat` for a new feature and `fix` for a bug fix.
- Other types are allowed. Choose a conventional type such as `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, or `revert` only when it truthfully describes the change.
- Write a short description immediately after the prefix. Prefer a concise, lowercase, command-style summary for consistency.
- Separate an optional free-form body from the description with one blank line. Use it to explain context or motivation that is not clear from the summary.
- Put optional footers after one blank line. Format each as `Token: value` or `Token #value`; replace spaces in tokens with hyphens.
- Mark a breaking change with `!` immediately before the colon, a `BREAKING CHANGE: <description>` footer, or both. `BREAKING-CHANGE` is an equivalent footer token. A breaking change may use any commit type.

Do not invent a scope, issue reference, breaking change, or motivation not supported by the diff or user context.

## Deliver or commit

- If asked only for a message, return the final message in a plain text code block, without XML or placeholder syntax.
- If asked to validate a message, identify specification violations separately from optional style improvements.
- If asked to commit, show or state the selected message and run `git commit` only after the requested changes are staged. Preserve multiline bodies and footers as separate paragraphs.
- Do not push, amend, rebase, or otherwise rewrite history unless explicitly requested.
