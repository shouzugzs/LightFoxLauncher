# Privacy and identity notes

This file records how the project handles personal information — both the
user's and the maintainer's — and what has been deliberately done to keep the
two separate.

## For users of the launcher

**The launcher collects nothing and reports nothing.**

* No telemetry, no analytics, no crash reporting, no update ping.
* It contacts only the domains it must, and only when you ask it to:

  | Domain | When |
  |---|---|
  | `piston-meta.mojang.com` | fetching the version list (on launch/install) |
  | `piston-data.mojang.com` | downloading game files |
  | `libraries.minecraft.net` | downloading libraries |
  | `resources.download.minecraft.net` | downloading assets |
  | `launchermeta.mojang.com` | downloading a Java runtime |
  | `meta.fabricmc.net` / `meta.quiltmc.org` | loader versions (下载 page) |
  | `api.modrinth.com` | mod search/download (下载 page) |

* **No Microsoft authentication exists.** The launcher passes
  `auth_access_token` as the literal string `"0"` — there are no credentials
  to store, leak, or transmit. Offline mode only.
* **The announcement feed is off by default.** With no URL configured, the
  launcher performs exactly zero network requests at startup beyond the version
  manifest, which itself is cached for 30 minutes and replaced by a built-in
  fallback list when offline.
* What it writes stays next to the executable: `.minecraft/` (the game data),
  `lfl-settings.ini` (your preferences), `lfl-seen.ini` (which announcements
  you have read). None of it leaves the machine.

## For the maintainer

The project is published under a collective name, deliberately:

* Copyright lines read `Copyright (c) 2025 LightFoxLauncher contributors` — no
  personal name appears in any source file, document, or binary.
* `Cargo.toml` has no `authors` field.
* CAA submissions go to an address published **only** in the release notes for
  the version that opens contributions — never committed to the repository.
* Commit authorship: when the repository is initialised, set
  `git config user.name` / `user.email` to the collective identity
  (`LightFoxLauncher contributors` / an address that is not personally
  identifying). Global git config is currently unset on the build machine, so
  nothing has ever been committed with a personal identity.

### Known exposure outside this repository

The maintainer's real name appears in **other, pre-existing GitHub repositories**
and cannot be recalled from there. Consequences and mitigations:

* The association between that name and this project is **not created by this
  repository** — no file here links the two, and none should.
* If unlinking matters, the practical options are GitHub's name-change and
  account-transfer tooling on those repositories, not edits here. Changing
  anything in this project cannot reduce that exposure.
* Do not add commits from a machine whose global git identity is the personal
  one; a single commit embeds the name permanently in history, and rewriting
  published history is far harder than avoiding it.

### Threat model note

The stated reason for this care is hostile attention directed at the maintainer
of a different launcher (PCL2). This project does not interact with PCL2, does
not link to it, and does not mention it in any user-facing string — keeping the
two projects' public surfaces disjoint is the only mitigation available here.
