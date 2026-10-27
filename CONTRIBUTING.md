# Contributing to LightFoxLauncher

Thanks for wanting to help. Before you write code, please read this page in
full — this project has an unusual contribution policy, and it is better to know
it up front than after you have finished a patch.

**Short version:** the project is **not accepting pull requests at this time**,
and when it does, contributions will require a **copyright assignment**
([`docs/CAA.md`](docs/CAA.md)), not just a DCO sign-off. Bug reports, ideas, and
reproductions are welcome today and carry no such requirement.

---

## 1. Current status: pull requests are closed

LightFoxLauncher is at an early stage and the maintainer is the only committer.
To keep the codebase coherent while the architecture is still moving, **pull
requests and patches are not being merged at this time.**

What that means in practice:

| You want to… | Can you? | How |
|---|---|---|
| Report a bug | **Yes** | Open an issue with the steps and the version |
| Suggest a feature | **Yes** | Open an issue; describe the problem, not just the solution |
| Ask a question | **Yes** | Open an issue |
| Send a patch / pull request | **Not yet** | See §2 |
| Translate the docs | **Ask first** | Open an issue before starting |

Opening a pull request while this policy is in force does **not** create an
obligation on either side. The maintainer may close it unread. **Please do not
send unsolicited patches** — a rejected patch is wasted work for you, and an
unread one sitting in a queue is worse than no patch at all.

This policy will be lifted explicitly, in this file and in the repository
description, if and when the project starts accepting contributions. Until you
see that change, assume the answer is no.

## 2. When pull requests do open

Two things will be required, in this order:

1. **Open an issue first** and agree on the approach. This avoids the most
   common waste: a large, well-written patch for a design the maintainer does not
   want. Anything touching `docs/CONTRACT.md` interfaces, the binary-size budget,
   or the zero-dependency rule needs agreement before code exists.
2. **Sign the CAA.** Read [`docs/CAA.md`](docs/CAA.md), fill in the signature
   table, and email it to the maintainer address published in the repository description or the latest release notes, with the subject
   `CAA: <your GitHub username>`.

The CAA text in this repository is a **draft** until pull requests open. It is
published now so that the terms are known before anyone spends time on a patch,
not to invite signatures — do not sign and send one while §1 still says patches
are closed. The draft has not been reviewed by a lawyer; it is written to be
plain enough to read without one.

### Why a CAA and not a DCO?

A DCO (Developer Certificate of Origin) is a *licence*: you keep your copyright
and grant the project permission to use the code. That is enough for most
projects, but it has a consequence this project cannot accept: **every
contributor keeps a permanent veto over relicensing.** If the project ever needs
to move to a newer MPL version, adopt a different licence, or resolve a
compatibility problem with a downstream user, it would have to track down and get
written permission from every person who ever contributed a line.

A CAA assigns the copyright to the project's copyright holder, which keeps the
project able to make that decision as a single legal person. You get a broad
licence back (§3 of the CAA), so you can still use and redistribute your own work
freely.

If you are not comfortable assigning copyright, that is a completely reasonable
position — but then the project cannot take your patch. Please keep sending bug
reports instead; they are just as valuable and carry no paperwork.

### Corporate contributions

If your employer owns the intellectual property in your work, the CAA must be
signed by someone authorised to bind the company, and the company block must be
filled in. Get that approval **before** you start writing code.

## 3. Bug reports

A good report is worth more than a patch. Please include:

- The `LFL-gui.exe` or `LFL.exe` version (`LFL -V`, or the title bar).
- What you did, what you expected, and what happened.
- The full error text. The launcher writes human-readable errors that name the
  file or URL involved — please paste them rather than paraphrasing.
- Whether the data directory is a fresh install or an existing one, and the
  Minecraft version involved.
- For download or verification problems: the Java runtime in use, and whether the
  problem reproduces with `LFL <version> --dry-run`.

Do **not** paste your Microsoft account credentials, session tokens, or the
contents of `launcher_accounts.json`. The launcher has no login of its own, but
other launchers store tokens in the same data directory.

## 4. Building and testing

```
cargo build --release      # builds LFL.exe and LFL-gui.exe
cargo test                 # 216 offline unit tests; no network required
```

Requirements: stable Rust 1.95.0 or newer, MSVC toolchain, Windows 8.1+.

There is no network access in the test suite by design. If your change needs a
network call to test, it needs a seam that lets the test run offline instead.

## 5. Project rules a patch must respect

These are not style preferences; a patch that breaks them will be rejected
regardless of quality.

1. **Zero dependencies.** `Cargo.lock` contains exactly one package: `lightfox`.
   No new entries in `[dependencies]`. Every parser, hash, and HTTP call is
   hand-written for this reason. Adding `serde`, `reqwest`, or similar defeats the
   entire premise of the project.
2. **Binary budget.** `LFL.exe` and `LFL-gui.exe` must each stay **under 1 MB**
   after a release build. Current sizes are in the README. A change that pushes
   either over budget needs to pay for itself.
3. **The module contract is frozen.** `docs/CONTRACT.md` defines the public
   interfaces each module exposes. Do not change those signatures without
   agreement; internal helpers are yours.
4. **`unsafe` is for Win32 FFI only.** The only permitted `unsafe` is documented
   FFI (`#[link(name = ...)] extern "system"`). No `unsafe` shortcuts in pure-Rust
   code.
5. **Errors are strings, not panics.** Fallible functions return
   `Res<T> = Result<T, String>` with a message naming the file or URL. No
   `unwrap()` on filesystem or network results.
6. **Tests stay green.** Every module has a `#[cfg(test)] mod tests` block.
7. **Licence headers stay intact.** Every source file starts with an
   `SPDX-License-Identifier: MPL-2.0` line. Do not remove or alter it (MPL-2.0
   §3.4 requires keeping it).

## 6. Code style

There is no formatter config; `cargo fmt` output is close enough. Beyond that:

- Comments explain **why**, not what. A comment that restates the code is noise.
  A comment that records the bug a line prevents is valuable.
- Prefer a plain loop and a helper function to a clever chain. This codebase is
  read far more often than it is written.
- User-facing strings are Simplified Chinese, matching the existing UI. Code,
  identifiers, and doc comments are English.
- The GUI's `layout()` and `paint()` must both derive positions from the shared
  metrics helpers — see the layout section of `docs/CONTRACT.md`. Hard-coded
  pixel offsets in either function will be rejected.

## 7. Licensing of the project itself

LightFoxLauncher is released under the **Mozilla Public License 2.0**. The full
text is in [`LICENSE`](LICENSE).

In plain terms:

- You may use, modify, and redistribute it, including commercially.
- If you distribute a modified version, the **modified files must stay under
  MPL-2.0** and you must make their source available. Files you add that are not
  derived from this project can be under any licence you like, as long as the
  combination does not restrict the MPL-covered parts.
- MPL-2.0 is file-level copyleft, not project-level: linking against the library
  from a separate program does not force that program under MPL-2.0.
- There is no warranty.

If you distribute a modified `LFL.exe` or `LFL-gui.exe`, MPL-2.0 §3.2 requires you
to make the corresponding source available and to tell recipients how to get it.

## 8. Code of conduct

Be straightforward and stay on the technical question. Disagreement about a
design is fine and useful; dismissiveness toward the person is not. The
maintainer may close threads that turn personal.

## 9. Questions about this policy

Open an issue titled `licensing: <question>`. Questions about the contribution
policy are welcome, including disagreement with it — the policy exists to keep
the project legally simple, not to shut people out.
