# ResearchWiki

Native desktop app that gathers academic articles from open sources (arXiv, PMC, PubMed, EuroPMC, medRxiv, bioRxiv, OpenAlex, Crossref, Unpaywall, Semantic Scholar, ClinicalTrials) and builds a navigable knowledge-graph wiki from them.

Single-binary Windows desktop app built on egui/eframe. Data lives at `%APPDATA%\ResearchWiki\`.

## Desktop features

- Gather articles manually from any supported source, or run all sources from the Gather tab.
- Track queued/running gather jobs with live counters, cancel active jobs, and inspect run history/events.
- Use Gather's smoke-test controls to run a source through gather + KG creation, start KG/wiki backfills, and verify the scheduler by arming a real next-minute scheduled run.
- Configure daily scheduled gathers from Settings.
- On Windows, minimizing the app hides it to the system tray while the in-process scheduler keeps running. Use the tray icon to restore the window or quit the app.

## Status

Early development. Forked from a server-mode predecessor; the native desktop UI is being rebuilt incrementally. No public release yet.

## Building from source

```
cargo build --release
```

Requires:
- Rust 1.83+
- Windows: Visual Studio Build Tools 2019+ with "Desktop development with C++" workload (for the bundled SQLite C source)

## Notes

- Scheduled gathers only run while the app process is alive.
- Closing or quitting the app stops the scheduler; minimizing on Windows keeps it running from the system tray.
- The Gather scheduler test temporarily changes one source schedule, waits for the normal scheduler loop to enqueue it, then restores the previous scheduler settings.

## License

MIT. See [LICENSE](LICENSE).
