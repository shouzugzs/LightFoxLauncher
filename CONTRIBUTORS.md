# Contributors

LightFoxLauncher is currently written and maintained by a single author. This file
records everyone whose contribution has been accepted into the project, so that
attribution does not depend on reading the git history.

## Maintainer

| Name | Role | Since |
|---|---|---|
| LightFoxLauncher contributors | Author, maintainer, copyright holder | 2025 |

## Accepted contributions

None yet. The project is not accepting pull requests at this time — see
[`CONTRIBUTING.md`](CONTRIBUTING.md) §1.

When contributions open, each accepted contributor is listed here after their
CAA ([`docs/CAA.md`](docs/CAA.md)) is on file:

| Name | GitHub | Contribution | CAA on file |
|---|---|---|---|
| _—_ | _—_ | _—_ | _—_ |

## Third-party components

**None.** The launcher links no external crate and vendors no third-party source.
`Cargo.lock` contains exactly one package, `lightfox` itself. Everything else —
JSON parsing, SHA-1, MD5, the ZIP reader, the DEFLATE decoder, and the HTTP client —
is implemented in this repository. See the "Why it is small" section of the
README for why that was done deliberately.

The Minecraft client, libraries, assets, and Java runtime that the launcher
*downloads* are not part of this project and are governed by their own licences
(Mojang's EULA for the game; the respective vendors' licences for the runtimes).
The launcher downloads them on the user's behalf but neither bundles nor
redistributes them.
