---
name: release
description: 'Release workflow for kvn-tui: bump version in Cargo.toml, PKGBUILD, AGENTS.md, regenerate Cargo.lock, commit, and tag.'
---

### Triggers

Activate this skill when the user says any of the following:
- "bump (major|minor|patch) version"
- "prepare release"
- "update release"
- "release X.Y.Z"

### Workflow

**Follow these steps exactly:**

1. **Read current version** from `Cargo.toml` (`version = "X.Y.Z"`).
2. **Determine new version:**
   - If user said **"patch"** → increment patch (`0.6.2` → `0.6.3`)
   - If user said **"minor"** → increment minor, reset patch to 0 (`0.6.2` → `0.7.0`)
   - If user said **"major"** → increment major, reset minor and patch to 0 (`0.6.2` → `1.0.0`)
   - If user provided an **explicit version** (e.g. "release 0.7.1") → use that version
3. **Update files** (replace the old version with the new one):
   - `Cargo.toml` — `version = "X.Y.Z"`
   - `pkg/arch/PKGBUILD` — `pkgver=X.Y.Z`
   - `AGENTS.md` — update the version in the Project Overview line (e.g. `` `kvn-tui` (v0.6.2) ``)
4. **Regenerate `Cargo.lock`** by running `cargo check`.
5. **Stage changes:**
   ```bash
   git add Cargo.toml Cargo.lock pkg/arch/PKGBUILD AGENTS.md
   ```
6. **Commit:**
   ```bash
   git commit -m "chore(release): bump version to X.Y.Z"
   ```
7. **Tag:**
   ```bash
   git tag -a vX.Y.Z -m "Release vX.Y.Z"
   ```
8. **Ask the user** whether to push:
   ```bash
   git push && git push origin vX.Y.Z
   ```

### Validation

- Ensure the version string follows semantic versioning (`MAJOR.MINOR.PATCH`).
- Do not create a release tag if the working tree is dirty (uncommitted changes exist).
- Always run `cargo check` so `Cargo.lock` is updated and the build is validated.
