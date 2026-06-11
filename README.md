# ResearchWiki

A native desktop app that gathers academic articles from open scholarly sources and
builds a navigable, LLM‑curated **knowledge‑graph wiki** from them — scoped to your own
research question.

Built in Rust with [egui/eframe](https://github.com/emilk/egui). Runs on **macOS** and
**Windows** (and Linux for development). It talks to any **OpenAI‑compatible** LLM and
embedding endpoint — hosted (OpenAI, etc.) or local (Ollama, LM Studio, llama.cpp, …).

## What it does

- **Gather** recent articles from 10 open sources for your topic — arXiv, PubMed, PMC,
  Europe PMC, medRxiv, bioRxiv, OpenAlex, Crossref, Semantic Scholar, and
  ClinicalTrials.gov. (Unpaywall is used to resolve open‑access PDFs.)
- **Screen, fetch, and evaluate** each candidate with your LLM, then embed it for
  semantic + keyword (hybrid) search.
- **Build a knowledge graph** of entities and relationships across the saved articles,
  and compile a **wiki** of synthesized entity articles.
- **Gap Bridge** turns your broad question + the graph's under‑connected areas into a
  refined, answerable next research question.
- **Multiple research sets** ("input sets"), each with its own articles, graph, wiki, and
  gather schedule.
- **Scheduling that fits a desktop app**: each research set can auto‑gather on a cadence
  (e.g. every 7 days), checked when you open the app and while it runs — no always‑on
  server needed.
- **Traces** tab to inspect every LLM call (prompt, tokens, latency, cost, errors).
- System‑tray support, light modern UI, and English/Korean interface.

## Requirements

**To run:** an OpenAI‑compatible **LLM** endpoint and **embedding** endpoint. These can be
a paid API (e.g. OpenAI) or a local server. You provide the base URL, model name, and API
key in the setup wizard.

**To build from source:**
- Rust 1.83+ (edition 2024).
- A C toolchain for the bundled SQLite:
  - **macOS:** Xcode Command Line Tools (`xcode-select --install`).
  - **Windows:** Visual Studio Build Tools 2019+ with the "Desktop development with C++" workload.
  - **Linux:** a C compiler (`build-essential` / `gcc`).

## Download

Pre-built desktop artifacts are attached to tagged releases:

Latest release: **v0.1.2**.

| Platform | File |
|---|---|
| macOS | `ResearchWiki-macos.dmg` |
| Windows | `ResearchWiki-windows.zip` |

On macOS, open the DMG and copy `ResearchWiki.app` to `/Applications`.
On Windows, unzip `ResearchWiki-windows.zip` and run `ResearchWiki.exe` from the extracted folder.

### What's new in v0.1.2

- Maintenance release with clippy cleanups in first-run endpoint scheme validation and
  arXiv/RSS source parsing.
- No user-facing workflow changes are expected; the setup wizard, gather pipeline, and
  existing v0.1.1 source reliability improvements remain in place.

## Build from source

```sh
git clone <repo-url>
cd researchwiki
cargo build --release
# binary: target/release/researchwiki
```

### macOS app bundle (recommended for end users)

A bundled `.app` launches from Finder with no terminal window and carries the app icon:

```sh
cargo install cargo-bundle      # one time
cargo bundle --release --bin researchwiki
# → target/release/bundle/osx/ResearchWiki.app
```

The bundled app icon uses `assets/app-icon.png`; the window/taskbar icon and
tray/menu-bar icon use `assets/researchwiki_icon.png`. The tray icon is
downscaled in memory for the platform tray API.

> On Windows, release builds run without a console window. On macOS, running the bare
> binary from a terminal will show logs; launch the bundled `.app` for a clean experience.

### Opening the app on macOS (Gatekeeper)

The macOS build is **ad‑hoc signed but not Apple‑notarized**, so the first launch shows an
"unidentified developer" / "cannot verify" warning. To open it:

- **Control‑click** (right‑click) **ResearchWiki-macos.dmg** or **ResearchWiki.app → Open**, then **Open** again in the dialog. macOS remembers this and won't ask again.
- Or go to **System Settings → Privacy & Security** and choose **Open Anyway** for ResearchWiki.

If macOS instead says the app is **"damaged and can't be opened"** (this happens with downloaded
apps because of the quarantine flag), move it to `/Applications` and clear the flag:

```sh
xattr -dr com.apple.quarantine "/Applications/ResearchWiki.app"
open "/Applications/ResearchWiki.app"
```

## First run

A short setup wizard appears on first launch:

1. **Connect** — your LLM and embedding endpoints (base URL, model, API key) and an
   optional contact email.
2. **Your research** — name it, state the question you're trying to answer, and list a few
   key topics / search terms.

You can change any of this later: research details in the **Input Set** tab, endpoints and
keys in **Settings**.

## Configuration

All settings are editable in‑app and saved to `settings.json` (locked to your user account;
`0600` on macOS/Linux). API keys are stored there too — not in any external service.

Environment variables (and a `.env` file) override the saved values, which is handy for
local/dev setups:

| Variable | Purpose |
|---|---|
| `LLM_BASE_URL`, `LLM_MODEL`, `LLM_API_KEY` | LLM endpoint (evaluation, screening, KG extraction, synthesis) |
| `EMBEDDING_BASE_URL`, `EMBEDDING_MODEL`, `EMBEDDING_API_KEY` | Embedding endpoint (defaults to OpenAI `text-embedding-3-small`; `OPENAI_API_KEY` works as a fallback) |
| `EMBEDDING_DIMENSIONS` | Embedding vector size (default 1536) |
| `RESEARCHWIKI_CONTACT_EMAIL` | Sent to polite‑pool APIs (OpenAlex, Crossref, Unpaywall). Without it, Unpaywall PDF resolution is skipped and no address is sent. |
| `SEMANTIC_SCHOLAR_API_KEY` | Enables the Semantic Scholar source (its keyless tier is too rate‑limited to use). Skipped when unset. |
| `DATABASE_PATH`, `PROMPTS_DIR`, `SETTINGS_FILE`, `WIKI_EXPORT_DIR` | Override storage locations |

LLM behavior can be tuned with `LLM_DISABLE_THINKING`, `LLM_REQUEST_TIMEOUT_SECONDS`,
`LLM_MAX_ATTEMPTS`, and `LLM_MAX_CONCURRENT_REQUESTS`. PDF→text extraction can use
[markitdown](https://github.com/microsoft/markitdown) via `MARKITDOWN_COMMAND`.

## Gathering & scheduling

- **Manually:** the **Gather** tab runs any single source or all of them; **Input Set →
  Save & start gathering** runs a gather for the active research set immediately.
- **On a cadence:** in **Input Set → Gathering schedule**, turn on *Auto‑gather every N
  days* and choose *Ask me first* or *Gather automatically*. The cadence is measured from
  the last gather and checked when you open the app and every 30 minutes while it's open —
  so it catches up the next time you launch (or fires live if you keep the app in the
  tray). Auto‑runs look back far enough to cover the gap since the last run.

Notes on sources: arXiv gather uses OAI-PMH plus the official RSS feed and spaces
requests politely; **Semantic Scholar** needs an API key (see above). You can sanity‑check
every source's connectivity with the bundled tool:

```sh
QUERY="diabetes" DAYS_BACK=365 cargo run --bin check_sources
```

## Data & privacy

- Everything is stored locally. There is no telemetry.
- Per‑user data directory:
  - **macOS:** `~/Library/Application Support/com.ResearchWiki.ResearchWiki/`
  - **Windows:** `%APPDATA%\ResearchWiki\ResearchWiki\data\`
  - **Linux:** `~/.local/share/ResearchWiki/`
- It holds `settings.json`, the workspace registry (`meta.db`), each research set's SQLite
  database, the prompt templates, and exported wiki files.
- Articles, prompts, and your research context are sent to the LLM/embedding endpoints you
  configure, and queries go to the public scholarly APIs above.

## System tray (macOS & Windows)

Minimizing hides the window to the tray / menu bar and keeps the app running, so scheduled
gathers can fire. Closing the window asks whether to **minimize to tray** or **quit**
(with a "don't ask again" option). Use the tray's **Open** to restore the window.
(There is no system tray on Linux; the app closes normally there.)

## Tabs

**Dashboard** (overview) · **Input Set** (research setup + schedule) · **Gather** ·
**Articles** · **Knowledge Graph** · **Wiki** · **Gap Bridge** · **Prompts** (edit the YAML
templates) · **Traces** (LLM call log) · **Settings**.

## Troubleshooting

- **arXiv returns "Rate exceeded" (429):** normal gathers use OAI-PMH/RSS instead of
  export search, but shared/datacenter IPs can still be throttled. Try again later.
- **PMC/NCBI ELink returns HTTP 500:** PMC articles still list and save; PubMed ID links
  are skipped for any ELink batch that fails.
- **Semantic Scholar returns nothing:** set `SEMANTIC_SCHOLAR_API_KEY` (free from
  semanticscholar.org) in Settings — the keyless tier is unusable.
- **"endpoint not set / invalid" on startup:** the setup wizard reappears if the saved LLM
  or embedding configuration is missing or malformed; re‑enter it.

## Development

- `cargo clippy --all-targets` and `cargo test` for checks.
- Set `RESEARCHWIKI_DEV=1` to reveal the Gather tab's advanced diagnostics.
- Dev binaries: `check_sources` (source health), `run_demo_gather`, `seed_diabetes_demo`,
  `eval`.

## License

MIT. See [LICENSE](LICENSE).
