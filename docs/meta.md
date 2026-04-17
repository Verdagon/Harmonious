# Documentation Strategy (META)

This document defines how documentation is organized across the erw project. It is the canonical source of truth for documentation structure and conventions.

## Directory Layout

Every major directory (the project root, each crate) can have a `docs/` subdirectory:

```
erw/
  docs/                              # Project-wide docs
    meta.md                          # This file
    historical/                      # Archived design docs, plans, and reports
  rustc-lang-facade/
    docs/                            # Library-specific docs
  toylangc/
    docs/                            # Consumer/example-specific docs
```

Docs live next to the code they describe. Project-wide concerns live in `erw/docs/`. Crate-specific concerns live in that crate's `docs/` directory.

## Document Categories

Every document belongs to exactly one category. The category determines where the doc lives, how it's discovered, and who its audience is.

For any category, the content lives in either a single file `docs/<category>.md` or, if there are multiple documents, in a subdirectory `docs/<category>/<topic>.md`.

### 1. Background

**Audience:** Anyone reading code in this area.

**Purpose:** General knowledge you need to understand what's going on when you encounter this feature in code. "Things you have to know to read code in this part of the codebase."

**Example:** What the four query providers are, what opaque stubs are, how the consumer/library split works.

**Discovery:** Background docs from the current directory and all ancestor directories should be listed in the nearest `CLAUDE.md`. Background knowledge is inherited — project-wide background is available everywhere, and crate-specific background is available within that crate.

**Location:** `docs/background.md` or `docs/background/<topic>.md`

### 2. Usage

**Audience:** Anyone writing code that interacts with this feature.

**Purpose:** How to use the feature correctly. Patterns, APIs, do's and don'ts. "Things you have to know to write code that interacts with this feature."

**Example:** How to add a new query provider, how to add a new AST node type, how to write an integration test.

**Discovery:** Symlinked into `.claude/rules/` so Claude auto-loads them when editing nearby files.

**Location:** `docs/usage.md` or `docs/usage/<topic>.md`

### 3. Arcana

**Audience:** Anyone debugging or writing code who encounters a non-obvious cross-cutting effect.

**Purpose:** Documents a local thing that has surprising, non-obvious effects elsewhere in the codebase. Each arcana has a unique ID (initialism + Z suffix) and `@ID` references at every affected code site.

**Discovery:** `@ID` comments in code point readers to the arcana doc. The doc lives in the `docs/` directory of the feature that *causes* the cross-cutting effect.

**Location:** `docs/arcana/<HammerCaseTitle>-<ID>.md` (e.g., `docs/arcana/GlobalMutexSerializesAllQueries-GMSAQZ.md`)

**ID convention:** Uppercase initialism of title words, Z suffix.

### 4. Shields

**Audience:** AI agents and reviewers enforcing code quality.

**Purpose:** Enforceable rules and constraints. Each shield has a unique ID (initialism + X suffix).

**Discovery:** Listed in `CLAUDE.md` as plain markdown links with descriptions from the shield's frontmatter `description:` field.

**Location:** `docs/shields/<HammerCaseTitle>-<ID>.md` (e.g., `docs/shields/NoStringBasedTypes-NSBTX.md`)

**ID convention:** Uppercase initialism of title words, X suffix.

### 5. Architecture

**Audience:** Anyone modifying the feature's own implementation.

**Purpose:** Internal design, data flow, invariants, and implementation details that a maintainer needs to understand before changing the feature. "Things you have to know to modify this feature's internals."

**Example:** How the two-pass codegen works (internal functions + extern wrappers), how deep dependency discovery walks the monomorphized instance graph.

**Discovery:** Symlinked into `.claude/rules/` for auto-loading when editing the feature's code.

**Location:** `docs/architecture.md` or `docs/architecture/<topic>.md`

### 6. Reasoning (sub-category of Architecture)

**Audience:** Anyone wondering "why is it done this way?"

**Purpose:** Records the alternatives considered and why the current approach was chosen. Lives alongside the architecture it explains.

**Location:** `docs/reasoning.md` or `docs/reasoning/<topic>.md`

### 7. Skills

**Audience:** AI agents executing specific processes.

**Purpose:** Step-by-step methodology for LLM-driven workflows.

**Discovery:** Lives in `docs/skills/`. Referenced by skill definitions in `.claude/skills/`.

**Location:** `docs/skills/<skill-name>.md`

### 8. Bugs

**Audience:** Anyone investigating known issues.

**Purpose:** Known bugs and limitations are documented as `#[ignore]`'d tests with explanatory comments describing the bug and expected behavior. Tests *are* the bug tracker.

**Location:** `#[ignore]`'d tests in code, not standalone documents.

### 9. Requirements

**Audience:** Anyone wondering what the system should do.

**Purpose:** Our tests serve as our requirements. They are the source of truth for what the system is expected to do.

**Location:** Tests in code, not standalone documents.

## Symlink Conventions

Categories #2 (Usage) and #5 (Architecture) are symlinked into `.claude/rules/` so Claude auto-loads them when editing nearby files. The symlink directory structure mirrors the source `docs/` structure:

```
.claude/rules/facade/architecture/queries.mdc  -->  ../../rustc-lang-facade/docs/architecture/queries.md
.claude/rules/toylang/usage/adding-ast-nodes.mdc  -->  ../../toylangc/docs/usage/adding-ast-nodes.md
```

Shields (#4) are NOT symlinked. They are listed in `CLAUDE.md` as plain markdown links with descriptions, so they are visible for reference but not auto-loaded into context.

The source of truth is always the `docs/` file. The `.mdc` symlink exists only for auto-loading.

## CLAUDE.md

Each directory can have a `CLAUDE.md` that includes:

- **Background docs (#1):** Imported from current directory and all ancestors. A file in `rustc-lang-facade/` sees project-wide background + facade-specific background.
- **Shield lists (#4):** Links to applicable shields with descriptions.

## Cross-References Between Categories

Docs link to more specific categories, forming a discovery chain:

- **Background** -> links to relevant **Usage** docs
- **Usage** -> links to relevant **Arcana** and **Shield** docs
- **Architecture** -> links to relevant **Reasoning** and **Skill** docs

Each link is a relative markdown link in a `## See also` section at the bottom of the doc. The good-doc skill maintains these when creating or updating docs.

## Existing Doc Classification

The following existing documents should be reclassified under this scheme:

| Location | Category |
|---|---|
| `docs/architecture/rust-interop-guide.md` | Architecture |
| `docs/architecture/known-tech-debt.md` | Architecture |
| `docs/reasoning/trait-call-investigation.md` | Reasoning |
| `docs/historical/` | (archive) — graduated plans and old reports |

## What Does NOT Get a Document

- **Inventories/catalogs** of structs, functions, or types. These are derivable from code and go stale.
- **Anything derivable from `git log` or `git blame`.**
- **Debugging solutions or fix recipes.** The fix is in the code; the commit message has the context.
- **Plans and proposals.** Historical phase-by-phase planning lives in `docs/historical/quest.md` (archived diary); ongoing investigation maps live in `future-architecture-investigations.md` at repo root. Plans that graduate into implementation get summarized into architecture (#5) docs.
